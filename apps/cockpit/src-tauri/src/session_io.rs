//! Session export / import: serialize a session's transcript and provider-turn
//! ledger to a portable JSON document, and re-import it as a new (archived)
//! session for viewing.

use crate::error::CmdError;
use ryuzi_core::domain::{NewMessage, NewProviderTurn, Session, SessionStatus};
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
                harness: "native".into(),
                model: None,
                effort: None,
                perm_mode: PermMode::Default,
                created_at: Some(0),
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
}
