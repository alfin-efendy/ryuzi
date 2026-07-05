//! The native turn drain: one `run_turn` runs a prompt to completion, calling
//! the model, executing tools, and persisting + streaming everything through
//! the same [`CoreEvent`] surface the ACP harness uses.

use super::ledger::Ledger;
use super::llm::LlmStream;
use super::permission::{evaluate, PermDecision};
use super::tools::{OutputCaps, ToolCtx, ToolRegistry};
use super::{context, NATIVE_ID};
use crate::approval::ApprovalHub;
use crate::domain::{CoreEvent, NewMessage, PermMode};
use crate::harness::TurnPrompt;
use crate::llm_router::client::MessageStreamEvent;
use crate::store::Store;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Upper bound on provider turns per drain, to bound runaway tool loops.
const MAX_PROVIDER_TURNS: usize = 50;
/// `max_tokens` requested per provider turn.
const MAX_TOKENS: i64 = 8192;
/// Flush the streaming-text buffer into a persisted row at this size or on a
/// newline, whichever comes first (keeps rows delta-shaped without spamming).
const TEXT_FLUSH_BYTES: usize = 120;

/// Everything one native session needs to run turns. Built by
/// [`super::NativeHarness::start_session`].
pub struct RunnerDeps {
    pub session_pk: String,
    pub work_dir: PathBuf,
    pub model: Option<String>,
    pub perm_mode: PermMode,
    pub project_policy: Option<String>,
    pub store: Arc<Store>,
    pub events: broadcast::Sender<CoreEvent>,
    pub approvals: Arc<ApprovalHub>,
    pub llm: Arc<dyn LlmStream>,
    pub tools: Arc<ToolRegistry>,
}

/// Run one prompt to completion. Returns `Ok(())` once the turn settles
/// (end_turn / cancellation); the control plane then emits `CoreEvent::Result`.
pub async fn run_turn(
    deps: &RunnerDeps,
    prompt: TurnPrompt,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    // 1. Persist + broadcast the user's message (display text).
    emit_row(
        deps,
        "user",
        "text",
        json!({ "text": prompt.display }),
        None,
        None,
        None,
    )
    .await;

    // 2. Load history and append the user turn to the ledger.
    let mut ledger = Ledger::load(deps.store.clone(), &deps.session_pk).await?;
    ledger
        .append_user(json!([{ "type": "text", "text": prompt.agent }]))
        .await?;

    let system = context::assemble_system(&deps.work_dir);
    let tool_defs = deps.tools.definitions();
    let model = deps.model.clone().unwrap_or_default();

    // 3. Provider-turn loop.
    for _ in 0..MAX_PROVIDER_TURNS {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let body = json!({
            "model": model,
            "system": system,
            "messages": ledger.messages(),
            "tools": tool_defs,
            "max_tokens": MAX_TOKENS,
            "stream": true,
        });

        let mut rx = deps.llm.stream(body).await?;
        let mut turn = TurnAccum::default();
        let mut text_buf = String::new();

        while let Some(item) = rx.recv().await {
            if cancel.is_cancelled() {
                // Mid-stream cancel: the assistant turn was not appended, so the
                // ledger still ends at the user turn — valid for a later resume.
                return Ok(());
            }
            let ev = match item {
                Ok(ev) => ev,
                Err(e) => {
                    flush_text(deps, &mut text_buf).await;
                    return Err(e);
                }
            };
            let Some(decoded) = MessageStreamEvent::from_event(&ev) else {
                continue;
            };
            match decoded {
                MessageStreamEvent::TextDelta { text, .. } => {
                    turn.text.push_str(&text);
                    text_buf.push_str(&text);
                    if text_buf.len() >= TEXT_FLUSH_BYTES || text_buf.contains('\n') {
                        flush_text(deps, &mut text_buf).await;
                    }
                }
                MessageStreamEvent::ThinkingDelta { text, .. } => {
                    emit_row(
                        deps,
                        "assistant",
                        "thought",
                        json!({ "text": text }),
                        None,
                        None,
                        None,
                    )
                    .await;
                }
                MessageStreamEvent::ContentBlockStart { index, block } => {
                    if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                        turn.tools.insert(
                            index,
                            ToolAccum {
                                id: block
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                name: block
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                start_input: block.get("input").cloned().unwrap_or(json!({})),
                                input_json: String::new(),
                            },
                        );
                    }
                }
                MessageStreamEvent::InputJsonDelta {
                    index,
                    partial_json,
                } => {
                    if let Some(t) = turn.tools.get_mut(&index) {
                        t.input_json.push_str(&partial_json);
                    }
                }
                MessageStreamEvent::MessageDelta { stop_reason, .. } => {
                    turn.stop_reason = stop_reason;
                }
                MessageStreamEvent::Error(msg) => {
                    flush_text(deps, &mut text_buf).await;
                    anyhow::bail!("{msg}");
                }
                MessageStreamEvent::MessageStop => break,
                MessageStreamEvent::MessageStart(_)
                | MessageStreamEvent::ContentBlockStop { .. } => {}
            }
        }
        flush_text(deps, &mut text_buf).await;

        // Assemble the assistant turn's content for the ledger.
        let mut content: Vec<Value> = Vec::new();
        if !turn.text.is_empty() {
            content.push(json!({ "type": "text", "text": turn.text }));
        }
        let tool_calls: Vec<ToolAccum> = turn.tools.into_values().collect();
        for t in &tool_calls {
            content.push(json!({
                "type": "tool_use",
                "id": t.id,
                "name": t.name,
                "input": t.parsed_input(),
            }));
        }
        // An empty assistant turn (no text, no tools) still needs a body.
        if content.is_empty() {
            content.push(json!({ "type": "text", "text": "" }));
        }
        ledger.append_assistant(json!(content)).await?;

        if tool_calls.is_empty() {
            return Ok(()); // end_turn (or max_tokens with no tools)
        }

        // Execute each tool call, collecting tool_result blocks.
        let mut results: Vec<Value> = Vec::new();
        for (i, t) in tool_calls.iter().enumerate() {
            if cancel.is_cancelled() {
                // Fill this and every remaining tool_use with an interrupted
                // result so the appended user turn stays provider-valid.
                for rest in &tool_calls[i..] {
                    results.push(tool_result(&rest.id, "Interrupted by user", true));
                }
                break;
            }
            results.push(run_tool_call(deps, t).await);
        }
        ledger.append_user(json!(results)).await?;

        if cancel.is_cancelled() {
            return Ok(());
        }
    }
    Ok(())
}

/// Insert the tool_call row, gate it, execute, and update the row. Returns the
/// Anthropic `tool_result` block to append to the ledger.
async fn run_tool_call(deps: &RunnerDeps, t: &ToolAccum) -> Value {
    let input = t.parsed_input();
    let Some(tool) = deps.tools.get(&t.name) else {
        let msg = format!("unknown tool `{}`", t.name);
        insert_tool_row(deps, t, &input, "unknown").await;
        finish_tool_row(deps, &t.id, &msg, true).await;
        return tool_result(&t.id, &msg, true);
    };
    insert_tool_row(deps, t, &input, tool.kind()).await;

    // Permission gate.
    let spec = tool.permission(&input);
    let decision = evaluate(
        &spec,
        deps.perm_mode,
        deps.project_policy.as_deref(),
        &deps.session_pk,
        &t.id,
        &deps.approvals,
        &deps.events,
    )
    .await;
    if decision == PermDecision::Deny {
        let msg = "Denied by user";
        finish_tool_row(deps, &t.id, msg, true).await;
        return tool_result(&t.id, msg, true);
    }

    // Execute.
    let ctx = ToolCtx {
        session_pk: deps.session_pk.clone(),
        work_dir: deps.work_dir.clone(),
        store: deps.store.clone(),
        cancel: CancellationToken::new(),
        caps: OutputCaps::default(),
    };
    match tool.execute(&ctx, input).await {
        Ok(out) => {
            finish_tool_row_with_display(deps, &t.id, &out.for_model, out.is_error, out.display)
                .await;
            tool_result(&t.id, &out.for_model, out.is_error)
        }
        Err(e) => {
            let msg = format!("{}: {e}", t.name);
            finish_tool_row(deps, &t.id, &msg, true).await;
            tool_result(&t.id, &msg, true)
        }
    }
}

/// Insert the initial `tool_call` row (`{name, input}`, in_progress).
async fn insert_tool_row(deps: &RunnerDeps, t: &ToolAccum, input: &Value, kind: &str) {
    emit_row(
        deps,
        "assistant",
        "tool_call",
        json!({ "name": t.name, "input": input }),
        Some(t.id.clone()),
        Some("in_progress".to_string()),
        Some(kind.to_string()),
    )
    .await;
}

/// Patch the tool_call row with its output + terminal status, then re-emit the
/// merged row with its ORIGINAL seq (the UI upserts by `tool_call_id`).
async fn finish_tool_row(deps: &RunnerDeps, tool_call_id: &str, output: &str, is_error: bool) {
    finish_tool_row_with_display(deps, tool_call_id, output, is_error, None).await;
}

async fn finish_tool_row_with_display(
    deps: &RunnerDeps,
    tool_call_id: &str,
    output: &str,
    is_error: bool,
    display: Option<Value>,
) {
    let status = if is_error { "failed" } else { "completed" };
    let mut patch = json!({ "output": output });
    if let Some(Value::Object(extra)) = display {
        for (k, v) in extra {
            patch[k] = v;
        }
    }
    match deps
        .store
        .update_tool_call(&deps.session_pk, tool_call_id, Some(status), &patch)
        .await
    {
        Ok((seq, payload, tool_kind)) => {
            let _ = deps.events.send(CoreEvent::Message {
                session_pk: deps.session_pk.clone(),
                seq,
                role: "assistant".into(),
                block_type: "tool_call".into(),
                payload,
                tool_call_id: Some(tool_call_id.to_string()),
                status: Some(status.to_string()),
                tool_kind,
            });
        }
        Err(e) => tracing::warn!("native: update_tool_call({tool_call_id}) failed: {e}"),
    }
}

/// Flush any buffered streaming text as one delta-shaped `text` row.
async fn flush_text(deps: &RunnerDeps, buf: &mut String) {
    if buf.is_empty() {
        return;
    }
    let text = std::mem::take(buf);
    emit_row(
        deps,
        "assistant",
        "text",
        json!({ "text": text }),
        None,
        None,
        None,
    )
    .await;
}

/// Persist a message row and broadcast the matching `CoreEvent::Message`.
async fn emit_row(
    deps: &RunnerDeps,
    role: &str,
    block_type: &str,
    payload: Value,
    tool_call_id: Option<String>,
    status: Option<String>,
    tool_kind: Option<String>,
) {
    let msg = NewMessage {
        session_pk: deps.session_pk.clone(),
        role: role.to_string(),
        block_type: block_type.to_string(),
        payload: payload.clone(),
        tool_call_id: tool_call_id.clone(),
        status: status.clone(),
        tool_kind: tool_kind.clone(),
    };
    match deps.store.insert_message(msg).await {
        Ok(seq) => {
            let _ = deps.events.send(CoreEvent::Message {
                session_pk: deps.session_pk.clone(),
                seq,
                role: role.to_string(),
                block_type: block_type.to_string(),
                payload,
                tool_call_id,
                status,
                tool_kind,
            });
        }
        Err(e) => tracing::warn!("native[{NATIVE_ID}]: insert_message failed: {e}"),
    }
}

fn tool_result(tool_use_id: &str, content: &str, is_error: bool) -> Value {
    json!({
        "type": "tool_result",
        "tool_use_id": tool_use_id,
        "content": content,
        "is_error": is_error,
    })
}

/// Accumulator for one streamed assistant turn.
#[derive(Default)]
struct TurnAccum {
    text: String,
    tools: BTreeMap<i64, ToolAccum>,
    stop_reason: Option<String>,
}

/// Accumulator for one streamed `tool_use` block.
struct ToolAccum {
    id: String,
    name: String,
    start_input: Value,
    input_json: String,
}

impl ToolAccum {
    /// The tool input: the streamed `input_json` if present, else the object
    /// carried on the `content_block_start`.
    fn parsed_input(&self) -> Value {
        if self.input_json.trim().is_empty() {
            return self.start_input.clone();
        }
        serde_json::from_str(&self.input_json).unwrap_or_else(|_| self.start_input.clone())
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::super::llm::LlmStream;
    use crate::llm_router::client::AnthropicEvent;
    use async_trait::async_trait;
    use serde_json::Value;
    use std::sync::Mutex;
    use tokio::sync::mpsc;

    /// An `LlmStream` that replays scripted turns: the first `stream()` call
    /// returns turn 0's events, the next returns turn 1's, and so on.
    pub struct ScriptedLlm {
        turns: Mutex<std::collections::VecDeque<Vec<AnthropicEvent>>>,
    }

    impl ScriptedLlm {
        pub fn new(turns: Vec<Vec<AnthropicEvent>>) -> Self {
            ScriptedLlm {
                turns: Mutex::new(turns.into_iter().collect()),
            }
        }
    }

    #[async_trait]
    impl LlmStream for ScriptedLlm {
        async fn stream(
            &self,
            _body: Value,
        ) -> anyhow::Result<mpsc::Receiver<anyhow::Result<AnthropicEvent>>> {
            let events = self
                .turns
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("ScriptedLlm: no more scripted turns"))?;
            let (tx, rx) = mpsc::channel(64);
            tokio::spawn(async move {
                for ev in events {
                    if tx.send(Ok(ev)).await.is_err() {
                        break;
                    }
                }
            });
            Ok(rx)
        }
    }

    /// Convenience: a text-delta event.
    pub fn text_delta(text: &str) -> AnthropicEvent {
        (
            "content_block_delta".into(),
            serde_json::json!({
                "type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": text}
            }),
        )
    }
    pub fn tool_use_start(index: i64, id: &str, name: &str) -> AnthropicEvent {
        (
            "content_block_start".into(),
            serde_json::json!({
                "type": "content_block_start", "index": index,
                "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}
            }),
        )
    }
    pub fn input_json_delta(index: i64, partial: &str) -> AnthropicEvent {
        (
            "content_block_delta".into(),
            serde_json::json!({
                "type": "content_block_delta", "index": index,
                "delta": {"type": "input_json_delta", "partial_json": partial}
            }),
        )
    }
    pub fn message_delta(stop_reason: &str) -> AnthropicEvent {
        (
            "message_delta".into(),
            serde_json::json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason},
                "usage": {"output_tokens": 1}
            }),
        )
    }
    pub fn message_stop() -> AnthropicEvent {
        (
            "message_stop".into(),
            serde_json::json!({"type": "message_stop"}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;
    use crate::domain::CoreEvent;
    use crate::store::Store;

    async fn deps_at(dir: &std::path::Path, llm: Arc<dyn LlmStream>) -> RunnerDeps {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());
        let (events, _rx) = broadcast::channel(256);
        RunnerDeps {
            session_pk: "s1".into(),
            work_dir: dir.to_path_buf(),
            // bypassPermissions so the scripted bash tool runs without a prompt.
            model: Some("test/model".into()),
            perm_mode: PermMode::BypassPermissions,
            project_policy: None,
            store,
            events,
            approvals: Arc::new(ApprovalHub::new()),
            llm,
            tools: Arc::new(ToolRegistry::builtin()),
        }
    }

    #[tokio::test]
    async fn full_turn_text_tool_use_result_then_end() {
        let dir = tempfile::tempdir().unwrap();
        // Turn 1: some text, then a bash tool_use writing a file.
        let turn1 = vec![
            text_delta("Working on it.\n"),
            tool_use_start(1, "call-1", "bash"),
            input_json_delta(1, "{\"command\":\"echo hi > out.txt\"}"),
            message_delta("tool_use"),
            message_stop(),
        ];
        // Turn 2: acknowledges and ends.
        let turn2 = vec![
            text_delta("Done."),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![turn1, turn2]));
        let deps = deps_at(dir.path(), llm).await;
        let mut rx = deps.events.subscribe();

        run_turn(
            &deps,
            TurnPrompt {
                agent: "please write out.txt".into(),
                display: "please write out.txt".into(),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

        // Side effect: the bash tool ran in the worktree.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("out.txt"))
                .unwrap()
                .trim(),
            "hi"
        );

        // Persisted display rows: user text, assistant text, tool_call (twice:
        // insert + update reuse same seq), assistant text "Done.".
        let msgs = deps.store.list_messages("s1").await.unwrap();
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].payload["text"], "please write out.txt");
        assert!(msgs.iter().any(|m| m.block_type == "text"
            && m.role == "assistant"
            && m.payload["text"]
                .as_str()
                .unwrap()
                .contains("Working on it")));
        let tool_row = msgs
            .iter()
            .find(|m| m.block_type == "tool_call")
            .expect("a tool_call row");
        assert_eq!(tool_row.payload["name"], "bash");
        assert_eq!(tool_row.status.as_deref(), Some("completed"));
        assert!(tool_row.payload.get("output").is_some());
        assert!(msgs
            .iter()
            .any(|m| m.block_type == "text" && m.payload["text"] == "Done."));

        // The provider-turn ledger is a valid alternating history:
        // user, assistant(text+tool_use), user(tool_result), assistant(text).
        let turns = deps.store.list_provider_turns("s1").await.unwrap();
        assert_eq!(turns.len(), 4);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[1].role, "assistant");
        assert!(turns[1]
            .payload
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["type"] == "tool_use"));
        assert_eq!(turns[2].role, "user");
        assert_eq!(turns[2].payload[0]["type"], "tool_result");
        assert_eq!(turns[3].role, "assistant");

        // A CoreEvent::Message was broadcast for the user row.
        let first = rx.try_recv();
        assert!(matches!(first, Ok(CoreEvent::Message { .. })));
    }

    #[tokio::test]
    async fn stream_error_propagates() {
        let dir = tempfile::tempdir().unwrap();
        let turn = vec![(
            "error".to_string(),
            json!({"type": "error", "error": {"message": "boom"}}),
        )];
        let llm = Arc::new(ScriptedLlm::new(vec![turn]));
        let deps = deps_at(dir.path(), llm).await;
        let err = run_turn(
            &deps,
            TurnPrompt {
                agent: "x".into(),
                display: "x".into(),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn precancelled_turn_returns_without_calling_model() {
        let dir = tempfile::tempdir().unwrap();
        // No scripted turns: if the loop called the model it would error.
        let llm = Arc::new(ScriptedLlm::new(vec![]));
        let deps = deps_at(dir.path(), llm).await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        run_turn(
            &deps,
            TurnPrompt {
                agent: "x".into(),
                display: "x".into(),
            },
            cancel,
        )
        .await
        .unwrap();
        // The user row was still persisted before the cancel check.
        let msgs = deps.store.list_messages("s1").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
    }
}
