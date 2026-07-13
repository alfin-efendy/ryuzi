//! `session_search` — recall across past sessions (spec §7.4), zero-LLM.
//!
//! Four actions layered over `Store`'s FTS5/lineage helpers:
//! - `discovery {query}`: FTS5 match across `messages_fts`, ranked and
//!   deduped by lineage root, excluding worker/review sessions and the
//!   caller's own conversation.
//! - `read {session_pk, seq}`: the ±5-message window around a discovery hit.
//! - `scroll {session_pk, from}`: pages further through a session's
//!   transcript once the model is reading it.
//! - `browse`: the most recent chat/project sessions, for when the model
//!   doesn't have a search term yet.
//!
//! Entirely read-only (no writes, no side effects) — no permission prompt,
//! and safe to leave available to sub-agents (unlike `task`/`memory`).

use super::{truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SessionSearch;

/// System-prompt guidance (folded into `context::assemble_system`) teaching
/// the model the discovery -> read flow.
pub const SESSION_SEARCH_GUIDANCE: &str = "\
Use `session_search` to recall prior work from PAST sessions before assuming \
something hasn't been done or asking the user to repeat themselves: \
action=discovery {query} finds relevant past conversations as ranked \
snippets, then action=read {session_pk, seq} on a promising hit reads the \
surrounding messages in full. action=scroll {session_pk, from} pages further \
through a session once you're reading it; action=browse lists your most \
recent sessions when you don't have a search term yet. This tool is \
read-only and never surfaces worker/review sub-sessions or your own current \
conversation.";

/// `read`'s window: this many messages on each side of the target `seq`.
const WINDOW_RADIUS: i64 = 5;
/// `scroll`'s page size, in messages.
const SCROLL_PAGE: usize = 40;
/// `browse`'s cap on returned sessions.
const BROWSE_LIMIT: usize = 20;
/// `discovery`'s cap on returned hits.
const DISCOVERY_LIMIT: i64 = 12;

#[async_trait]
impl Tool for SessionSearch {
    fn name(&self) -> &str {
        "session_search"
    }
    fn description(&self) -> &str {
        "Search your PAST sessions (not this one) for relevant prior work. \
         action=discovery {query} lists matching sessions ranked by \
         recency, as text snippets; action=read {session_pk, seq} returns \
         the messages surrounding a hit; action=scroll {session_pk, from} \
         pages further through a session's transcript; action=browse lists \
         your most recent sessions. Read-only; excludes worker/review \
         sub-sessions and your own current conversation."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["discovery", "read", "scroll", "browse"]},
                "query": {"type": "string", "description": "discovery: full-text search terms."},
                "session_pk": {"type": "string", "description": "read/scroll: which past session to look at."},
                "seq": {"type": "integer", "description": "read: the message seq to center the window on."},
                "from": {"type": "integer", "description": "scroll: the seq to page forward from (default 0)."}
            },
            "required": ["action"]
        })
    }
    fn kind(&self) -> &'static str {
        "search"
    }
    fn permission(&self, _input: &Value) -> PermissionSpec {
        // Read-only recall over the model's own history — no worktree or
        // system access, so it never needs a permission prompt.
        PermissionSpec::new("read", "search past sessions")
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let action = input
            .get("action")
            .and_then(|a| a.as_str())
            .unwrap_or("discovery");
        match action {
            "discovery" => discovery(ctx, &input).await,
            "read" => read(ctx, &input).await,
            "scroll" => scroll(ctx, &input).await,
            "browse" => browse(ctx).await,
            other => Ok(ToolOutput::error(format!(
                "session_search: unknown action `{other}` (discovery|read|scroll|browse)"
            ))),
        }
    }
}

async fn discovery(ctx: &ToolCtx, input: &Value) -> anyhow::Result<ToolOutput> {
    let Some(q) = input
        .get("query")
        .and_then(|q| q.as_str())
        .filter(|q| !q.trim().is_empty())
    else {
        return Ok(ToolOutput::error(
            "session_search: `query` is required for action=discovery",
        ));
    };
    // Exclude the CALLING session's own lineage — recall is for PAST
    // sessions, not the current conversation.
    let lineage = ctx
        .store
        .lineage_of(&ctx.session_pk)
        .await
        .unwrap_or_default();
    let hits = match ctx
        .store
        .search_messages_fts(q, &lineage, DISCOVERY_LIMIT)
        .await
    {
        Ok(hits) => hits,
        // `query` is model-supplied FTS5 syntax; a malformed expression
        // (e.g. an unterminated quote) errors here rather than panicking —
        // surface it as a normal tool error so the model can retry.
        Err(e) => {
            return Ok(ToolOutput::error(format!(
                "session_search: query `{q}` is not a valid search — {e}"
            )))
        }
    };
    if hits.is_empty() {
        return Ok(ToolOutput::ok("No matching past sessions."));
    }
    let body = hits
        .iter()
        .map(|h| {
            format!(
                "- {} [{}] seq {}: {}",
                h.title.as_deref().unwrap_or("(untitled)"),
                h.session_pk,
                h.seq,
                h.snippet
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ToolOutput::ok(truncate(&body, &ctx.caps)))
}

async fn read(ctx: &ToolCtx, input: &Value) -> anyhow::Result<ToolOutput> {
    let Some(pk) = input.get("session_pk").and_then(|v| v.as_str()) else {
        return Ok(ToolOutput::error(
            "session_search: `session_pk` is required for action=read",
        ));
    };
    let seq = input.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
    let window = match ctx.store.messages_window(pk, seq, WINDOW_RADIUS).await {
        Ok(w) => w,
        Err(e) => {
            return Ok(ToolOutput::error(format!(
                "session_search: read failed: {e}"
            )))
        }
    };
    if window.is_empty() {
        return Ok(ToolOutput::ok(format!(
            "No messages found for session `{pk}` around seq {seq}."
        )));
    }
    Ok(ToolOutput::ok(truncate(
        &render_window(pk, &window),
        &ctx.caps,
    )))
}

async fn scroll(ctx: &ToolCtx, input: &Value) -> anyhow::Result<ToolOutput> {
    let Some(pk) = input.get("session_pk").and_then(|v| v.as_str()) else {
        return Ok(ToolOutput::error(
            "session_search: `session_pk` is required for action=scroll",
        ));
    };
    let from = input
        .get("from")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);
    let all = match ctx.store.list_messages(pk).await {
        Ok(m) => m,
        Err(e) => {
            return Ok(ToolOutput::error(format!(
                "session_search: scroll failed: {e}"
            )))
        }
    };
    if all.is_empty() {
        return Ok(ToolOutput::ok(format!(
            "Session `{pk}` has no messages (or does not exist)."
        )));
    }
    let page: Vec<_> = all
        .iter()
        .filter(|m| m.seq > from)
        .take(SCROLL_PAGE)
        .cloned()
        .collect();
    if page.is_empty() {
        return Ok(ToolOutput::ok(format!(
            "End of session `{pk}` — no more messages after seq {from}."
        )));
    }
    let mut body = render_window(pk, &page);
    if let Some(last) = page.last() {
        if all.iter().any(|m| m.seq > last.seq) {
            body.push_str(&format!(
                "\n\n… more available — call again with action=scroll, from={}.",
                last.seq
            ));
        }
    }
    Ok(ToolOutput::ok(truncate(&body, &ctx.caps)))
}

async fn browse(ctx: &ToolCtx) -> anyhow::Result<ToolOutput> {
    // Same lineage-exclusion contract as discovery: don't list the caller's
    // own current conversation among its own past sessions.
    let lineage = ctx
        .store
        .lineage_of(&ctx.session_pk)
        .await
        .unwrap_or_default();
    let mut sessions = Vec::new();
    for kind in ["chat", "project"] {
        match ctx.store.list_sessions_by_kind(kind).await {
            Ok(mut s) => sessions.append(&mut s),
            Err(e) => {
                return Ok(ToolOutput::error(format!(
                    "session_search: browse failed: {e}"
                )))
            }
        }
    }
    sessions.retain(|s| !lineage.contains(&s.session_pk));
    sessions.sort_by_key(|s| std::cmp::Reverse(s.created_at));
    sessions.truncate(BROWSE_LIMIT);
    if sessions.is_empty() {
        return Ok(ToolOutput::ok("No past sessions yet."));
    }
    let body = sessions
        .iter()
        .map(|s| {
            format!(
                "- {} [{}] {}",
                s.title.as_deref().unwrap_or("(untitled)"),
                s.session_pk,
                s.kind.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ToolOutput::ok(truncate(&body, &ctx.caps)))
}

/// Render a message window/page as one `[seq] role/block_type: text` line
/// per message, headed by the session it came from.
fn render_window(session_pk: &str, messages: &[crate::domain::Message]) -> String {
    let mut out = format!("session {session_pk}:\n");
    for m in messages {
        let text = m
            .payload
            .get("text")
            .and_then(|t| t.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| m.payload.to_string());
        out.push_str(&format!(
            "[{}] {}/{}: {}\n",
            m.seq, m.role, m.block_type, text
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;
    use crate::domain::{NewMessage, PermMode, Session, SessionKind, SessionStatus};

    async fn seed_session(
        ctx: &super::super::ToolCtx,
        pk: &str,
        title: &str,
        kind: SessionKind,
        created_at: i64,
    ) {
        ctx.store
            .insert_session(Session {
                session_pk: pk.into(),
                primary_agent_id: None,
                primary_agent_snapshot: None,
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: Some(title.into()),
                status: SessionStatus::Idle,
                perm_mode: PermMode::Default,
                started_by: None,
                created_at: Some(created_at),
                last_active: Some(created_at),
                resume_attempts: 0,
                branch_owned: false,
                kind,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn discovery_lists_hits_and_read_returns_window() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        seed_session(&ctx, "chat-1", "t-chat-1", SessionKind::Chat, 1000).await;
        for i in 0..12 {
            ctx.store
                .insert_message(NewMessage::block(
                    "chat-1",
                    "user",
                    "text",
                    json!({ "text": format!("msg {i} about kubernetes ingress routing") }),
                ))
                .await
                .unwrap();
        }

        let out = SessionSearch
            .execute(&ctx, json!({"action": "discovery", "query": "ingress"}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("t-chat-1"), "{}", out.for_model);
        assert!(out.for_model.contains("chat-1"), "{}", out.for_model);

        // seqs run 1..=12 (12 inserts); a ±5 window around seq 6 is [1,11].
        let out = SessionSearch
            .execute(
                &ctx,
                json!({"action": "read", "session_pk": "chat-1", "seq": 6}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("msg 0"), "{}", out.for_model);
        assert!(out.for_model.contains("msg 10"), "{}", out.for_model);
        assert!(!out.for_model.contains("msg 11"), "{}", out.for_model);
    }

    #[tokio::test]
    async fn discovery_non_matching_query_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        seed_session(&ctx, "chat-1", "t-chat-1", SessionKind::Chat, 1000).await;
        ctx.store
            .insert_message(NewMessage::block(
                "chat-1",
                "user",
                "text",
                json!({ "text": "kubernetes ingress routing" }),
            ))
            .await
            .unwrap();

        let out = SessionSearch
            .execute(
                &ctx,
                json!({"action": "discovery", "query": "nonexistentzzz"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("No matching"), "{}", out.for_model);
    }

    #[tokio::test]
    async fn discovery_malformed_query_returns_clean_error_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        // An unterminated quote is invalid FTS5 query syntax.
        let out = SessionSearch
            .execute(
                &ctx,
                json!({"action": "discovery", "query": "\"unterminated"}),
            )
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.for_model);
        assert!(
            out.for_model.contains("not a valid search"),
            "{}",
            out.for_model
        );
    }

    #[tokio::test]
    async fn discovery_without_query_is_a_clean_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = SessionSearch
            .execute(&ctx, json!({"action": "discovery"}))
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("`query`"), "{}", out.for_model);
    }

    #[tokio::test]
    async fn read_with_no_messages_reports_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = SessionSearch
            .execute(
                &ctx,
                json!({"action": "read", "session_pk": "does-not-exist", "seq": 1}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(
            out.for_model.contains("No messages found"),
            "{}",
            out.for_model
        );
    }

    #[tokio::test]
    async fn scroll_pages_and_reports_a_next_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        for i in 0..(super::SCROLL_PAGE + 5) {
            ctx.store
                .insert_message(NewMessage::block(
                    "chat-1",
                    "user",
                    "text",
                    json!({ "text": format!("line {i}") }),
                ))
                .await
                .unwrap();
        }
        let out = SessionSearch
            .execute(&ctx, json!({"action": "scroll", "session_pk": "chat-1"}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("line 0"), "{}", out.for_model);
        assert!(
            out.for_model.contains("more available"),
            "{}",
            out.for_model
        );

        // Paging from the reported cursor reaches the tail with no cursor.
        let out = SessionSearch
            .execute(
                &ctx,
                json!({"action": "scroll", "session_pk": "chat-1", "from": super::SCROLL_PAGE as i64}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(
            !out.for_model.contains("more available"),
            "{}",
            out.for_model
        );
    }

    #[tokio::test]
    async fn browse_lists_recent_sessions_and_excludes_the_caller() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ctx_at(dir.path()).await;
        ctx.session_pk = "self".into();
        seed_session(&ctx, "self", "t-self", SessionKind::Chat, 3000).await;
        seed_session(&ctx, "chat-old", "t-old", SessionKind::Chat, 1000).await;
        seed_session(&ctx, "chat-new", "t-new", SessionKind::Chat, 2000).await;
        seed_session(&ctx, "wrk-1", "t-worker", SessionKind::Worker, 2500).await;

        let out = SessionSearch
            .execute(&ctx, json!({"action": "browse"}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("chat-old"), "{}", out.for_model);
        assert!(out.for_model.contains("chat-new"), "{}", out.for_model);
        assert!(!out.for_model.contains("self"), "{}", out.for_model);
        assert!(!out.for_model.contains("wrk-1"), "{}", out.for_model);
        // Most-recent-first.
        let new_pos = out.for_model.find("chat-new").unwrap();
        let old_pos = out.for_model.find("chat-old").unwrap();
        assert!(new_pos < old_pos, "{}", out.for_model);
    }

    #[tokio::test]
    async fn unknown_action_is_a_clean_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = SessionSearch
            .execute(&ctx, json!({"action": "teleport"}))
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.for_model);
        assert!(
            out.for_model.contains("unknown action"),
            "{}",
            out.for_model
        );
    }
}
