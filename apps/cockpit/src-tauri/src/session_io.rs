//! Session export / import: serialize a session's transcript and provider-turn
//! ledger to a portable JSON document, and re-import it as a new (archived)
//! session for viewing.

use crate::error::CmdError;
use ryuzi_core::domain::{Message, NewMessage, NewProviderTurn, Session, SessionStatus};
use ryuzi_core::paths::{new_id, now_ms};
use ryuzi_core::{ControlPlane, Store};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;

#[derive(Serialize, Deserialize)]
struct MessageExport {
    role: String,
    block_type: String,
    payload: Value,
    tool_call_id: Option<String>,
    status: Option<String>,
    tool_kind: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct TurnExport {
    role: String,
    payload: Value,
}

#[derive(Serialize, Deserialize)]
struct SessionExport {
    version: u32,
    title: Option<String>,
    messages: Vec<MessageExport>,
    provider_turns: Vec<TurnExport>,
}

async fn build_export(store: &Store, session_pk: &str) -> anyhow::Result<SessionExport> {
    let session = store
        .get_session(session_pk)
        .await?
        .ok_or_else(|| anyhow::anyhow!("unknown session {session_pk}"))?;
    let messages = store.list_messages(session_pk).await?;
    let turns = store.list_provider_turns(session_pk).await?;
    Ok(SessionExport {
        version: 1,
        title: session.title,
        messages: messages
            .into_iter()
            .map(|m| MessageExport {
                role: m.role,
                block_type: m.block_type,
                payload: m.payload,
                tool_call_id: m.tool_call_id,
                status: m.status,
                tool_kind: m.tool_kind,
            })
            .collect(),
        provider_turns: turns
            .into_iter()
            .map(|t| TurnExport {
                role: t.role,
                payload: t.payload,
            })
            .collect(),
    })
}

async fn apply_import(
    store: &Store,
    project_id: &str,
    export: SessionExport,
) -> anyhow::Result<Session> {
    let session = Session {
        session_pk: new_id(),
        project_id: project_id.to_string(),
        agent_session_id: None,
        worktree_path: None,
        branch: None,
        title: export.title,
        status: SessionStatus::Ended,
        started_by: Some("import".to_string()),
        created_at: Some(now_ms()),
        last_active: Some(now_ms()),
        resume_attempts: 0,
        // Imported sessions never own a branch to clean up on end.
        branch_owned: false,
    };
    store.insert_session(session.clone()).await?;
    for m in export.messages {
        store
            .insert_message(NewMessage {
                session_pk: session.session_pk.clone(),
                role: m.role,
                block_type: m.block_type,
                payload: m.payload,
                tool_call_id: m.tool_call_id,
                status: m.status,
                tool_kind: m.tool_kind,
            })
            .await?;
    }
    for t in export.provider_turns {
        store
            .insert_provider_turn(NewProviderTurn::new(
                session.session_pk.clone(),
                t.role,
                t.payload,
            ))
            .await?;
    }
    Ok(session)
}

/// Export a session as a pretty JSON string.
#[tauri::command]
#[specta::specta]
pub async fn export_session(cp: State<'_, Arc<ControlPlane>>, session_pk: String) -> R<String> {
    let export = build_export(cp.store(), &session_pk).await?;
    Ok(serde_json::to_string_pretty(&export).map_err(anyhow::Error::from)?)
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Render a session's transcript to a self-contained, shareable HTML document.
fn build_html(title: &str, messages: &[Message]) -> String {
    let mut body = String::new();
    for m in messages {
        let content = match m.block_type.as_str() {
            "tool_call" => {
                let name = m
                    .payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool");
                let out = m
                    .payload
                    .get("output")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                format!(
                    "<strong>{}</strong>\n{}",
                    escape_html(name),
                    escape_html(out)
                )
            }
            "status" => escape_html(
                m.payload
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            ),
            _ => escape_html(m.payload.get("text").and_then(|v| v.as_str()).unwrap_or("")),
        };
        if content.trim().is_empty() {
            continue;
        }
        body.push_str(&format!(
            "<div class=\"msg {}\"><div class=\"role\">{}</div><pre class=\"content\">{}</pre></div>\n",
            escape_html(&m.block_type),
            escape_html(&m.role),
            content
        ));
    }
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{title}</title><style>\
         body{{font:14px/1.5 system-ui,sans-serif;max-width:800px;margin:2rem auto;padding:0 1rem;color:#222}}\
         h1{{font-size:1.3rem}}.msg{{margin:1rem 0;border-left:3px solid #ddd;padding-left:.75rem}}\
         .role{{font-weight:600;font-size:.75rem;text-transform:uppercase;color:#888}}\
         .content{{white-space:pre-wrap;font:inherit;margin:.25rem 0 0}}\
         .msg.tool_call{{border-color:#7c3aed}}.msg.status{{border-color:#16a34a}}\
         </style></head><body><h1>{escaped_title}</h1>{body}\
         <footer style=\"margin-top:2rem;color:#aaa;font-size:.75rem\">Shared from ryuzi</footer>\
         </body></html>",
        title = escape_html(title),
        escaped_title = escape_html(title),
        body = body
    )
}

/// Render a session as a self-contained, shareable HTML document.
#[tauri::command]
#[specta::specta]
pub async fn share_session(cp: State<'_, Arc<ControlPlane>>, session_pk: String) -> R<String> {
    let session = cp
        .store()
        .get_session(&session_pk)
        .await?
        .ok_or_else(|| CmdError::from(anyhow::anyhow!("unknown session {session_pk}")))?;
    let messages = cp.list_messages(&session_pk).await?;
    let title = session.title.unwrap_or_else(|| "ryuzi session".to_string());
    Ok(build_html(&title, &messages))
}

/// Import a previously exported session JSON as a new archived session.
#[tauri::command]
#[specta::specta]
pub async fn import_session(
    cp: State<'_, Arc<ControlPlane>>,
    project_id: String,
    data: String,
) -> R<Session> {
    let export: SessionExport = serde_json::from_str(&data)
        .map_err(|e| CmdError::from(anyhow::anyhow!("invalid session file: {e}")))?;
    Ok(apply_import(cp.store(), &project_id, export).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryuzi_core::domain::{PermMode, Project};

    async fn store() -> Store {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        Store::open(tmp.path()).await.unwrap()
    }

    #[tokio::test]
    async fn export_then_import_round_trips_transcript_and_ledger() {
        let store = store().await;
        store
            .insert_project(Project {
                project_id: "p".into(),
                name: "p".into(),
                workdir: "/tmp".into(),
                source: None,
                model: None,
                effort: None,
                perm_mode: PermMode::Default,
                created_at: Some(0),
                is_git: false,
            })
            .await
            .unwrap();
        store
            .insert_session(Session {
                session_pk: "s1".into(),
                project_id: "p".into(),
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: Some("My session".into()),
                status: SessionStatus::Idle,
                started_by: None,
                created_at: Some(0),
                last_active: Some(0),
                resume_attempts: 0,
                branch_owned: false,
            })
            .await
            .unwrap();
        store
            .insert_message(NewMessage::block(
                "s1",
                "user",
                "text",
                serde_json::json!({ "text": "hello" }),
            ))
            .await
            .unwrap();
        store
            .insert_provider_turn(NewProviderTurn::new(
                "s1",
                "user",
                serde_json::json!([{"type": "text", "text": "hello"}]),
            ))
            .await
            .unwrap();

        // Export → JSON → import as a new session.
        let export = build_export(&store, "s1").await.unwrap();
        let json = serde_json::to_string(&export).unwrap();
        let parsed: SessionExport = serde_json::from_str(&json).unwrap();
        let imported = apply_import(&store, "p", parsed).await.unwrap();

        assert_ne!(imported.session_pk, "s1");
        assert_eq!(imported.title.as_deref(), Some("My session"));
        let msgs = store.list_messages(&imported.session_pk).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].payload["text"], "hello");
        let turns = store
            .list_provider_turns(&imported.session_pk)
            .await
            .unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].payload[0]["text"], "hello");
    }

    #[test]
    fn build_html_renders_and_escapes() {
        let messages = vec![
            Message {
                session_pk: "s".into(),
                seq: 1,
                role: "user".into(),
                block_type: "text".into(),
                payload: serde_json::json!({ "text": "hi <script>" }),
                tool_call_id: None,
                status: None,
                tool_kind: None,
                created_at: 0,
            },
            Message {
                session_pk: "s".into(),
                seq: 2,
                role: "assistant".into(),
                block_type: "tool_call".into(),
                payload: serde_json::json!({ "name": "bash", "output": "done" }),
                tool_call_id: Some("t1".into()),
                status: Some("completed".into()),
                tool_kind: Some("execute".into()),
                created_at: 0,
            },
        ];
        let html = build_html("My & Session", &messages);
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("My &amp; Session"));
        // User text is escaped (no live <script>).
        assert!(html.contains("hi &lt;script&gt;"));
        assert!(!html.contains("<script>"));
        // Tool call renders name + output.
        assert!(html.contains("<strong>bash</strong>"));
        assert!(html.contains("done"));
        assert!(html.contains("Shared from ryuzi"));
    }
}
