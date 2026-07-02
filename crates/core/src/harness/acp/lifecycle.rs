//! ACP session lifecycle: `session/new`, `session/set_mode`, `session/prompt`.
//!
//! Task 2 owns the three lifecycle round-trips that follow a successful
//! `initialize` (Task 1):
//!
//! - [`new_session`] — create a session on the agent, get back a `SessionId`
//!   plus the offered modes.
//! - [`set_mode`] — switch the session to one of the offered modes; guarded so
//!   we only send if the requested id appears in `available_modes`.
//! - [`prompt`] — send a user turn and wait for the `StopReason` that signals
//!   completion.
//!
//! A small helper [`perm_mode_to_acp_mode`] maps ryuzi's `PermMode` enum to
//! the ACP wire mode id strings the adapter understands.

use std::path::PathBuf;

use agent_client_protocol::schema::v1::{
    ContentBlock, LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse, SetSessionModeRequest, SetSessionModeResponse, SessionId,
    StopReason, TextContent,
};
use agent_client_protocol::ConnectionTo;
use agent_client_protocol_schema::v1::{McpServer, Usage, AGENT_METHOD_NAMES};

use crate::domain::PermMode;

/// Map a ryuzi [`PermMode`] to the ACP wire mode-id string the agent expects.
///
/// `Plan` is intentionally omitted: `PermMode::Plan` does not exist yet in 3A.
pub fn perm_mode_to_acp_mode(mode: PermMode) -> &'static str {
    match mode {
        PermMode::Default => "default",
        PermMode::AcceptEdits => "acceptEdits",
        PermMode::BypassPermissions => "bypassPermissions",
    }
}

/// Send `session/new` and return the full response (carries `session_id` +
/// optional `modes`).
pub async fn new_session(
    cx: &ConnectionTo<agent_client_protocol::Agent>,
    cwd: PathBuf,
    mcp_servers: Vec<McpServer>,
) -> Result<NewSessionResponse, agent_client_protocol::Error> {
    let session: NewSessionResponse = cx
        .send_request(NewSessionRequest::new(cwd).mcp_servers(mcp_servers))
        .block_task()
        .await
        .map_err(|err| {
            let message = format!(
                "ACP {} failed: {err}",
                AGENT_METHOD_NAMES.session_new
            );
            agent_client_protocol::Error::internal_error().data(message)
        })?;
    Ok(session)
}

/// Resume an existing session via `session/load` (cookbook §6). **Gated on
/// `supports_load`** — the caller passes the top-level `agent_capabilities.load_session`
/// bool read from the `initialize` response; if the agent does not advertise the
/// capability this returns an error rather than sending an unsupported request.
///
/// During load the agent replays the session's history as a stream of
/// `session/update` notifications (which the wired [`crate::harness::acp::notification`]
/// sink persists), then responds with a `LoadSessionResponse`.
pub async fn load_session(
    cx: &ConnectionTo<agent_client_protocol::Agent>,
    supports_load: bool,
    session_id: SessionId,
    cwd: PathBuf,
    mcp_servers: Vec<McpServer>,
) -> Result<LoadSessionResponse, agent_client_protocol::Error> {
    if !supports_load {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("agent does not advertise session/load (loadSession=false)"));
    }

    let response: LoadSessionResponse = cx
        .send_request(LoadSessionRequest::new(session_id, cwd).mcp_servers(mcp_servers))
        .block_task()
        .await
        .map_err(|err| {
            let message = format!("ACP {} failed: {err}", AGENT_METHOD_NAMES.session_load);
            agent_client_protocol::Error::internal_error().data(message)
        })?;
    Ok(response)
}

/// Send `session/set_mode` **only if** `mode_id` is listed in the session's
/// `available_modes`. A no-op (returns `Ok(())`) when the session advertised no
/// modes or the requested id is absent — callers should treat missing mode
/// support as "stay on current mode".
pub async fn set_mode(
    cx: &ConnectionTo<agent_client_protocol::Agent>,
    session_id: SessionId,
    mode_id: &str,
    available_modes: &[agent_client_protocol::schema::v1::SessionMode],
) -> Result<(), agent_client_protocol::Error> {
    // Guard: only send if the agent lists this mode.
    let offered = available_modes.iter().any(|m| m.id.0.as_ref() == mode_id);
    if !offered {
        return Err(agent_client_protocol::Error::invalid_params().data(format!(
            "mode '{mode_id}' is not in the agent's available_modes"
        )));
    }

    let _: SetSessionModeResponse = cx
        .send_request(SetSessionModeRequest::new(session_id, mode_id.to_string()))
        .block_task()
        .await
        .map_err(|err| {
            let message = format!(
                "ACP {} rejected: {err}",
                AGENT_METHOD_NAMES.session_set_mode
            );
            agent_client_protocol::Error::internal_error().data(message)
        })?;
    Ok(())
}

/// Send `session/prompt` with `content` and wait for the agent to finish the
/// turn. Returns `(StopReason, Option<Usage>)` — callers decide what to do
/// with the stop reason but need not match on specific variants.
pub async fn prompt(
    cx: &ConnectionTo<agent_client_protocol::Agent>,
    session_id: SessionId,
    content: Vec<ContentBlock>,
) -> Result<(StopReason, Option<Usage>), agent_client_protocol::Error> {
    let response: PromptResponse = cx
        .send_request(PromptRequest::new(session_id, content))
        .block_task()
        .await
        .map_err(|err| {
            let message = format!(
                "ACP {} failed: {err}",
                AGENT_METHOD_NAMES.session_prompt
            );
            agent_client_protocol::Error::internal_error().data(message)
        })?;
    Ok((response.stop_reason, response.usage))
}

/// Build a single `ContentBlock::Text` from a plain string — the minimal
/// content type for a user prompt.
pub fn text_block(text: impl Into<String>) -> ContentBlock {
    ContentBlock::Text(TextContent::new(text.into()))
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn new_session_set_mode_and_prompt_round_trip() {
        // drive: connect -> initialize -> new_session -> set_mode("default") -> prompt("hi")
        let outcome = crate::harness::acp::testkit::drive_lifecycle("default", "hi")
            .await
            .unwrap();
        assert!(!outcome.session_id.0.is_empty());
        assert!(outcome.completed, "prompt returned a StopReason");
    }

    #[tokio::test]
    async fn load_session_replays_transcript_into_the_store() {
        // drive: connect -> initialize -> session/load; the mock replays a
        // user + agent message chunk during load, which the sink persists.
        let (store, session_pk) =
            crate::harness::acp::testkit::drive_load("resume-abc").await;
        let msgs = store.list_messages(&session_pk).await.unwrap();
        assert!(
            msgs.iter().any(|m| m.role == "assistant"
                && m.block_type == "text"
                && m.payload["text"] == "previous answer"),
            "expected replayed assistant text row from session/load, got: {msgs:?}"
        );
    }
}
