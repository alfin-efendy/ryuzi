//! Shared Learning commands retained after the per-agent migration: cross-session
//! recall plus global skill-usage and pin controls.

use super::{ok, params, ApiError};
use crate::serve::ApiState;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &["search_sessions", "list_skill_usage", "set_skill_pinned"];

/// `search_sessions`'s cap on returned hits — generous enough for a Learning
/// panel search box, small enough to stay a single quick round trip.
const SEARCH_LIMIT: i64 = 50;
#[derive(Deserialize)]
struct QueryP {
    query: String,
}

#[derive(Deserialize)]
struct SetPinnedP {
    name: String,
    pinned: bool,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "search_sessions" => {
            let a: QueryP = params(p)?;
            let hits = cp
                .store()
                .search_messages_fts(&a.query, &[], SEARCH_LIMIT)
                .await
                .map_err(|e| ApiError::bad_request(e.to_string()))?;
            ok(hits)
        }
        "list_skill_usage" => ok(cp.store().list_skill_usage().await?),
        "set_skill_pinned" => {
            let a: SetPinnedP = params(p)?;
            cp.store().set_skill_pinned(&a.name, a.pinned).await?;
            ok(())
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

#[cfg(test)]
mod tests {
    use crate::api::{dispatch, tests_support::state};
    use serde_json::json;

    // ---------- dispatch (RPC surface), one per HANDLES method ----------

    #[tokio::test]
    async fn search_sessions_dispatches_and_finds_a_seeded_message() {
        let s = state().await;
        s.cp.store()
            .insert_session(crate::domain::Session {
                session_pk: "chat-1".into(),
                primary_agent_id: None,
                primary_agent_snapshot: None,
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: Some("t-chat-1".into()),
                status: crate::domain::SessionStatus::Idle,
                perm_mode: crate::domain::PermMode::Default,
                started_by: None,
                created_at: Some(1000),
                last_active: Some(1000),
                resume_attempts: 0,
                branch_owned: false,
                kind: crate::domain::SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
        s.cp.store()
            .insert_message(crate::domain::NewMessage::block(
                "chat-1",
                "user",
                "text",
                json!({ "text": "kubernetes ingress routing" }),
            ))
            .await
            .unwrap();

        let out = dispatch(&s, "search_sessions", json!({ "query": "ingress" }))
            .await
            .unwrap();
        let hits = out.as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["sessionPk"], "chat-1");
    }

    #[tokio::test]
    async fn search_sessions_surfaces_a_malformed_query_as_bad_request() {
        let s = state().await;
        let err = dispatch(&s, "search_sessions", json!({ "query": "\"unterminated" }))
            .await
            .unwrap_err();
        assert_eq!(err.status, 400);
    }

    #[tokio::test]
    async fn list_skill_usage_dispatches_and_decodes_as_an_array() {
        let s = state().await;
        s.cp.store().record_skill_use("deploy").await.unwrap();
        let out = dispatch(&s, "list_skill_usage", json!({})).await.unwrap();
        let rows = out.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "deploy");
    }

    #[tokio::test]
    async fn set_skill_pinned_dispatches_and_persists() {
        let s = state().await;
        s.cp.store().record_skill_use("deploy").await.unwrap();
        dispatch(
            &s,
            "set_skill_pinned",
            json!({ "name": "deploy", "pinned": true }),
        )
        .await
        .unwrap();
        let usage =
            s.cp.store()
                .get_skill_usage("deploy")
                .await
                .unwrap()
                .unwrap();
        assert!(usage.pinned);
    }
}
