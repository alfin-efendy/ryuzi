//! The native turn drain: one `run_turn` runs a prompt to completion, calling
//! the model, executing tools, and persisting + streaming everything through
//! the same [`CoreEvent`] surface the ACP harness uses.

use super::agents::{Agent, AgentRegistry};
use super::commands::CommandRegistry;
use super::context_manager::{
    compaction::CompactionOutcome, is_context_overflow, ContextConfig, ContextManager,
};
use super::iteration_budget::{IterationBudget, PARENT_MAX_ITERS, SUBAGENT_MAX_ITERS};
use super::llm::LlmStream;
use super::permission::{evaluate, PermDecision};
use super::steer::SteerBuffer;
use super::tools::{
    OutputCaps, SubagentSpawner, SubtaskResult, SubtaskSpec, SubtaskStatus, ToolCtx, ToolRegistry,
};
use super::{context, NATIVE_ID};
use crate::approval::ApprovalHub;
use crate::domain::{CoreEvent, NewMessage, PermMode};
use crate::harness::TurnPrompt;
use crate::llm_router::client::MessageStreamEvent;
use crate::store::Store;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Default upper bound on provider turns per drain, to bound runaway tool
/// loops. Overridable via the `agent.max_provider_turns` setting (floor 1).
/// Used as the default for the auto-continue window size / notice text inside
/// `drive()`; the parent budget itself is seeded in `run_turn` (defaulting to
/// [`PARENT_MAX_ITERS`], Phase 2's raised ceiling).
const DEFAULT_MAX_PROVIDER_TURNS: usize = 50;
/// Flush the streaming-text buffer into a persisted row at this size or on a
/// newline, whichever comes first (keeps rows delta-shaped without spamming).
const TEXT_FLUSH_BYTES: usize = 120;

/// Everything one native session needs to run turns. Built by
/// [`super::NativeHarness::start_session`]. Cloneable so a sub-agent spawner
/// can carry a copy.
#[derive(Clone)]
pub struct RunnerDeps {
    pub session_pk: String,
    pub work_dir: PathBuf,
    /// Session attachments folder (second read root for the `read` tool).
    pub attachments_dir: Option<PathBuf>,
    /// Plugin-bundled skill directories folded in beside the worktree/global
    /// ones (see `crate::plugins::PluginHost::enabled_skill_dirs`).
    pub extra_skill_dirs: Vec<PathBuf>,
    pub model: Option<String>,
    /// Reasoning effort for this session (from project settings; None = default).
    pub effort: Option<String>,
    /// Resolved per-model metadata (context window, max output, capabilities).
    pub meta: crate::llm_router::model_meta::ModelMeta,
    /// Interior-mutable so a LIVE session can pick up a permission-mode change
    /// (from the composer / project settings) on the NEXT turn without being
    /// torn down — the control plane refreshes it in the continue path. The
    /// tool gate reads it fresh per call via [`RunnerDeps::current_perm_mode`].
    pub perm_mode: Arc<std::sync::Mutex<PermMode>>,
    /// The owning project (for tool_policies lookups/writes). `None` only in
    /// bare test contexts without a session row.
    pub project_id: Option<String>,
    /// Per-session "don't ask again" sets, applied by the permission gate.
    pub perm_overrides: Arc<std::sync::Mutex<super::permission::SessionPermOverrides>>,
    pub store: Arc<Store>,
    pub events: broadcast::Sender<CoreEvent>,
    pub approvals: Arc<ApprovalHub>,
    pub llm: Arc<dyn LlmStream>,
    pub tools: Arc<ToolRegistry>,
    /// The selected primary agent for this session.
    pub agent: Agent,
    /// Available agents (for sub-agent spawning).
    pub agents: Arc<AgentRegistry>,
    /// Available slash commands.
    pub commands: Arc<CommandRegistry>,
    /// Persistent memory (None in contexts without a session row, e.g. bare
    /// tests, and always None inside sub-agents).
    pub memory: Option<Arc<super::memory::MemoryStore>>,
    /// Worktree snapshot stack for the `revert` tool (most recent last).
    pub snapshots: Arc<tokio::sync::Mutex<Vec<String>>>,
    /// Mid-turn steering buffer (Task B3). Cloned from `NativeSession::steer`
    /// at session start — the SAME buffer, not a fresh one — so a `steer()`
    /// call reaches whichever turn is currently draining it. Survives across
    /// turns: `refresh_turn_model` clones the whole `RunnerDeps` per turn, but
    /// `SteerBuffer`'s clone shares the underlying `Arc<Mutex<Vec<_>>>`.
    pub steer: SteerBuffer,
}

impl RunnerDeps {
    /// The current permission mode, read fresh at each tool gate so a
    /// mid-session mode change (refreshed by the control plane on continue)
    /// takes effect on the next tool call.
    pub fn current_perm_mode(&self) -> PermMode {
        *self.perm_mode.lock().expect("perm_mode mutex poisoned")
    }

    /// Overwrite the live permission mode (called by the control plane before
    /// each continued turn so composer/project-settings changes take effect).
    pub fn set_perm_mode(&self, mode: PermMode) {
        *self.perm_mode.lock().expect("perm_mode mutex poisoned") = mode;
    }
}

/// Run one prompt to completion. Returns `Ok(())` once the turn settles
/// (end_turn / cancellation); the control plane then emits `CoreEvent::Result`.
///
/// Resolves a leading slash command (expanding its template and any agent
/// override), persists the user's display row, then drives the agentic loop.
pub async fn run_turn(
    deps: &RunnerDeps,
    prompt: TurnPrompt,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    // /compact is an ACTION, not a prompt template: intercept before command
    // resolution so it never becomes model input.
    let trimmed = prompt.display.trim();
    if trimmed == "/compact" || trimmed.starts_with("/compact ") {
        return run_manual_compact(deps, &prompt).await;
    }

    // Slash-command resolution on the raw user text.
    let (agent_text, agent) = match deps.commands.resolve(&prompt.display) {
        Some((expanded, override_agent)) => {
            let agent = override_agent
                .and_then(|n| deps.agents.get(&n))
                .unwrap_or_else(|| deps.agent.clone());
            (merge_agent_prompt_suffix(expanded, &prompt), agent)
        }
        None => (prompt.agent.clone(), deps.agent.clone()),
    };

    // 1. Persist + broadcast the user's message (raw display text).
    emit_row(
        deps,
        "user",
        "text",
        user_row_payload(&prompt),
        None,
        None,
        None,
    )
    .await;

    // Per-turn model snapshot: re-read the project's pinned model fresh from
    // the store and resolve it for THIS turn, so a picker change mid-chat
    // applies on the next turn without restarting the session. Everything
    // below — request bodies, compaction, title generation, and the sub-agent
    // spawner — reads the snapshot; the original `deps` is never mutated, so
    // in-flight turns and running subagents keep the model they started with.
    // Only `model` is refreshed here; `meta` (context window / output caps /
    // prompt-cache support) stays at the session-start value.
    let turn_deps = refresh_turn_model(deps).await;
    let deps = &turn_deps;

    // 2. Load history + context state and append the user turn.
    let cfg = ContextConfig::load(&deps.store, deps.meta).await;
    let mut cm = ContextManager::load(deps.store.clone(), &deps.session_pk, cfg).await?;
    // Seed the indicator immediately on resume, before any model call —
    // prefer the persisted last-known status (server truth) over the
    // reload estimate (spec §6.1/§10).
    match deps.store.get_session_context(&deps.session_pk).await {
        Ok(Some(saved)) => {
            // Seed BEFORE reading status: an overflowed prior turn persisted
            // the full-window total via `mark_full`, but this fresh
            // `ContextManager` only knows the (possibly much smaller) reload
            // estimate, which would otherwise let `needs_compaction` miss the
            // overflow and loop forever (spec §12).
            if let Some(saved_active) = saved["active_tokens"].as_u64() {
                cm.seed_active_tokens(saved_active);
            }
            let st = cm.status();
            let _ = deps.events.send(CoreEvent::ContextUsage {
                session_pk: deps.session_pk.clone(),
                active_tokens: saved["active_tokens"].as_u64().unwrap_or(st.active_tokens),
                context_window: st.context_window,
                usable_window: saved["usable_window"].as_u64().unwrap_or(st.usable_window),
                percent_left: saved["percent_left"]
                    .as_u64()
                    .unwrap_or(st.percent_left as u64) as u8,
                cache_read_tokens: 0,
                output_tokens: 0,
            });
            // Re-emit the accumulated session cost from what's already
            // persisted — no accumulation here, just pricing the saved tally
            // at current rates (spec: resume must not double-count).
            let tally = super::cost::Tally::from_payload(&saved);
            if !tally.is_empty() {
                emit_session_cost(deps, &tally).await;
            }
        }
        // No persisted tally yet (fresh session) or a read error — either
        // way this is a display re-emit, never an accumulation: `cm` hasn't
        // committed any response yet, so `cm.last_*` would be all-zero at
        // best and stale at worst. `emit_context_usage` would otherwise
        // persist a spurious zero-token model entry (and a `total_usd=0`
        // `SessionCost`) on every brand-new session.
        _ => emit_context_display(deps, &cm, true).await,
    }
    cm.append_user(user_content_blocks(&prompt.blocks, &agent_text))
        .await?;

    // 3. Drive the loop with a spawner available for the `task` tool.
    let spawn: Arc<dyn SubagentSpawner> = Arc::new(RunnerSpawner {
        deps: deps.clone(),
        cancel: cancel.clone(),
        depth: 0,
    });
    // Seed the parent turn-cap from the `agent.max_provider_turns` setting,
    // defaulting to Phase 2's raised ceiling (PARENT_MAX_ITERS). This is what
    // makes the setting meaningful under the IterationBudget model: drive()'s
    // `while budget.try_consume()` loop caps at exactly this many provider
    // turns per window, and each auto-continue re-grants a fresh window of the
    // same size (drive() re-reads the setting for that grant).
    let max_provider_turns =
        crate::settings::usize_setting(&deps.store, "agent.max_provider_turns", PARENT_MAX_ITERS)
            .await;
    let budget = IterationBudget::new(max_provider_turns);
    drive(
        deps,
        &agent,
        &mut cm,
        &cancel,
        Some(spawn),
        DisplayMode::Full,
        &budget,
    )
    .await?;

    // 4. Best-effort: give a fresh session a generated title.
    maybe_generate_title(deps, &prompt.display).await;
    Ok(())
}

/// Manual /compact: persist the user's row, compact the session history, and
/// record a notice row. No model turn runs.
async fn run_manual_compact(deps: &RunnerDeps, prompt: &TurnPrompt) -> anyhow::Result<()> {
    emit_row(
        deps,
        "user",
        "text",
        user_row_payload(prompt),
        None,
        None,
        None,
    )
    .await;
    let cfg = ContextConfig::load(&deps.store, deps.meta).await;
    let mut cm = ContextManager::load(deps.store.clone(), &deps.session_pk, cfg).await?;
    // Same resume-seed as `run_turn` (spec §12): honor a persisted
    // post-overflow total so a manual /compact right after an overflow
    // reports honest `before_tokens` instead of the reload's undercount.
    if let Ok(Some(saved)) = deps.store.get_session_context(&deps.session_pk).await {
        if let Some(saved_active) = saved["active_tokens"].as_u64() {
            cm.seed_active_tokens(saved_active);
        }
    }
    if cm.is_empty() {
        emit_row(
            deps,
            "system",
            "notice",
            json!({ "text": "Nothing to compact yet." }),
            None,
            None,
            None,
        )
        .await;
        return Ok(());
    }
    let model = deps.model.clone().unwrap_or_default();
    let cmodel = super::llm::aux_model(&deps.store, "compaction", &model).await;
    match cm.compact(&deps.llm, &cmodel, "manual").await {
        Ok(outcome) => {
            emit_compaction(deps, "manual", &outcome, true).await;
            // Display-only: `compact()` never calls `commit_response()`, so
            // `cm.last_*` still hold whatever the last real assistant turn
            // committed (or nothing, if none has run yet this session) —
            // re-accumulating them here would double-count that response.
            emit_context_display(deps, &cm, true).await;
            Ok(())
        }
        Err(e) => {
            emit_row(
                deps,
                "system",
                "notice",
                json!({ "text": format!("Compaction failed: {e}") }),
                None,
                None,
                None,
            )
            .await;
            Ok(())
        }
    }
}

/// The persisted user-row payload: `{text}` plus `attachments` display
/// metadata when the turn carried any.
pub(crate) fn user_row_payload(prompt: &TurnPrompt) -> Value {
    let mut payload = json!({ "text": prompt.display });
    if !prompt.attachments.is_empty() {
        payload["attachments"] = Value::Array(prompt.attachments.clone());
    }
    payload
}

/// The Anthropic user-content array: image blocks first, then the text block.
pub(crate) fn user_content_blocks(blocks: &[Value], agent_text: &str) -> Value {
    let mut content = blocks.to_vec();
    content.push(json!({ "type": "text", "text": agent_text }));
    Value::Array(content)
}

fn merge_agent_prompt_suffix(expanded: String, prompt: &TurnPrompt) -> String {
    if prompt.agent == prompt.display {
        return expanded;
    }
    let Some(suffix) = prompt.agent.strip_prefix(&prompt.display) else {
        return expanded;
    };
    let suffix = suffix.trim();
    if suffix.is_empty() {
        expanded
    } else {
        format!("{expanded}\n\n{suffix}")
    }
}

/// Build this turn's `RunnerDeps`: a clone of `deps` carrying the freshest
/// resolution of the project's pinned model. Falls back to the session-start
/// model when no project row is reachable (bare tests, ephemeral contexts) or
/// when nothing resolves at all (empty store / no routable connection), so
/// those contexts behave exactly as before. When the pinned model fails
/// routing and a substitute is resolved, a status row announces the
/// substitution — no silent swap.
async fn refresh_turn_model(deps: &RunnerDeps) -> RunnerDeps {
    let pinned = match project_pinned_model(deps).await {
        Some(pinned) => pinned,
        None => deps.model.clone(),
    };
    let resolved = super::resolve_native_model(&deps.store, pinned.clone()).await;
    if let (Some(pinned), Some(resolved)) = (pinned.as_deref(), resolved.as_deref()) {
        if !pinned.trim().is_empty() && pinned != resolved {
            emit_row(
                deps,
                "system",
                "status",
                json!({
                    "summary":
                        format!("model `{pinned}` is not routable, using `{resolved}`")
                }),
                None,
                None,
                None,
            )
            .await;
        }
    }
    let mut turn = deps.clone();
    if resolved.is_some() {
        turn.model = resolved;
    }
    turn
}

/// `Some(project.model)` when the session's project row is reachable — the
/// inner Option is the pin itself, which may legitimately be unset. `None`
/// when there is no session/project row to read, or the session has no
/// bound project (chat-first sessions).
async fn project_pinned_model(deps: &RunnerDeps) -> Option<Option<String>> {
    let session = deps
        .store
        .get_session(&deps.session_pk)
        .await
        .ok()
        .flatten()?;
    let project = deps
        .store
        .get_project(&session.project_id?)
        .await
        .ok()
        .flatten()?;
    Some(project.model)
}

/// If this session has no title yet, generate a terse one from the first
/// prompt via a short non-streaming model call. Best-effort: any failure is
/// swallowed so it never affects the turn's outcome.
async fn maybe_generate_title(deps: &RunnerDeps, first_prompt: &str) {
    match deps.store.get_session(&deps.session_pk).await {
        Ok(Some(session)) if session.title.is_none() => {}
        _ => return, // no session row, or already titled
    }
    let model = super::llm::aux_model(
        &deps.store,
        "title",
        &deps.model.clone().unwrap_or_default(),
    )
    .await;
    if model.is_empty() {
        return;
    }
    let body = json!({
        "model": model,
        "max_tokens": 32,
        "system": "Generate a terse 3-6 word title (no quotes, no trailing punctuation) \
                   for a coding session that begins with the user's request. \
                   Reply with ONLY the title.",
        "messages": [{"role": "user", "content": [{"type": "text", "text": first_prompt}]}],
        "stream": true,
    });
    let Ok(title) = super::llm::collect_text(&deps.llm, body).await else {
        return;
    };
    let title: String = title.trim().trim_matches('"').chars().take(80).collect();
    if !title.is_empty() {
        let _ = deps.store.set_session_title(&deps.session_pk, &title).await;
    }
}

/// What a [`drive`] loop persists/streams to the transcript.
#[derive(Clone, Debug, PartialEq)]
enum DisplayMode {
    /// Parent turn: text, thoughts, tools, notices, context usage.
    Full,
    /// Sub-agent: only tool rows, tagged with the sub-agent's label. Text,
    /// thinking, notices, and context usage stay internal (the report arrives
    /// via the parent's `task` tool output).
    ToolsOnly { label: String },
}

impl DisplayMode {
    /// Text/thought/notice/context/compaction rows are shown (parent only).
    fn text(&self) -> bool {
        matches!(self, DisplayMode::Full)
    }
    /// Sub-agent attribution label for tool rows, if any.
    fn subagent(&self) -> Option<&str> {
        match self {
            DisplayMode::ToolsOnly { label } => Some(label),
            DisplayMode::Full => None,
        }
    }
}

/// Hermes' verbatim nudge for the post-exhaustion summary call: asks for a
/// final answer without inviting another round of tool calls.
const BUDGET_EXHAUSTED_PROMPT: &str = "You've reached the maximum number of \
    tool-calling iterations allowed. Please provide a final response \
    summarizing what you've found and accomplished so far, without calling \
    any more tools.";

/// The agentic provider-turn loop. Shared by the top-level turn and sub-agents.
/// `display` gates persistence of display rows: sub-agents stream only their
/// tool rows (tagged with their label) so their text/thinking stay internal.
/// Returns the final assistant text.
async fn drive(
    deps: &RunnerDeps,
    agent: &Agent,
    cm: &mut ContextManager,
    cancel: &CancellationToken,
    spawn: Option<Arc<dyn SubagentSpawner>>,
    display: DisplayMode,
    budget: &IterationBudget,
) -> anyhow::Result<String> {
    let system = match &agent.prompt {
        Some(p) => p.clone(),
        None => {
            let memory = deps.memory.as_ref().and_then(|m| m.snapshot());
            context::assemble_system(&deps.work_dir, &deps.extra_skill_dirs, memory.as_deref())
        }
    };
    // Tools restricted to what this agent may use.
    let tool_defs: Vec<Value> = deps
        .tools
        .definitions()
        .into_iter()
        .filter(|d| {
            d.get("name")
                .and_then(|n| n.as_str())
                .map(|n| agent.tools.allows(n))
                .unwrap_or(false)
        })
        .collect();
    let model = deps.model.clone().unwrap_or_default();
    let mut final_text = String::new();

    cm.set_baseline(&system, &tool_defs);
    let settings_cap =
        crate::settings::usize_setting(&deps.store, "context.max_output_tokens", 1).await;
    // usize_setting floors at 1; treat 1 (the "unset" default) as no cap.
    let max_tokens = if settings_cap > 1 {
        (deps.meta.max_output_tokens as usize).min(settings_cap) as i64
    } else {
        deps.meta.max_output_tokens as i64
    };
    let thinking_budget = thinking_budget(deps.effort.as_deref(), &deps.meta, max_tokens);

    // Window size for the auto-continue notice text and the fresh grant made on
    // each auto-continue (`agent.max_provider_turns`). The parent budget itself
    // is seeded from the same setting in `run_turn` (defaulting to
    // PARENT_MAX_ITERS); this read defaults to DEFAULT_MAX_PROVIDER_TURNS and is
    // only consulted on the top-level auto-continue path.
    let max_turns = crate::settings::usize_setting(
        &deps.store,
        "agent.max_provider_turns",
        DEFAULT_MAX_PROVIDER_TURNS,
    )
    .await;
    // Auto-continue is a top-level convenience only; sub-agents keep the hard
    // stop. Read without usize_setting's floor so "0" can disable it.
    let auto_budget = if display.text() {
        deps.store
            .get_setting("agent.auto_continue_budget")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(4)
    } else {
        0
    };

    // Composition of two loop-control features:
    //   * The consumable `IterationBudget` (Phase 2) is THE turn cap — the
    //     caller seeds it from `agent.max_provider_turns`; `try_consume()`
    //     bounds one window and housekeeping turns can `refund()`.
    //   * Auto-continue (#100) layers on top: when a window is spent without an
    //     end_turn, the top-level loop re-grants a fresh window (up to
    //     `auto_budget` times) so long runs finish without a user nudge.
    // The outer `loop` exists solely to let a refunded budget resume the
    // `while budget.try_consume()` window after an auto-continue.
    let mut auto_continue = 0usize;
    let mut provider_turn = 0usize;
    loop {
        while budget.try_consume() {
            if cancel.is_cancelled() {
                return Ok(final_text);
            }
            // Pre-turn (iteration 0) / mid-turn compaction check (spec §7.1).
            if cm.status().needs_compaction {
                let trigger = if provider_turn == 0 {
                    "pre_turn"
                } else {
                    "mid_turn"
                };
                let cmodel = super::llm::aux_model(&deps.store, "compaction", &model).await;
                match cm.compact(&deps.llm, &cmodel, trigger).await {
                    Ok(outcome) => emit_compaction(deps, trigger, &outcome, display.text()).await,
                    Err(e) => {
                        tracing::warn!("native: compaction failed, continuing uncompacted: {e}");
                        if display.text() {
                            emit_row(
                                deps,
                                "system",
                                "notice",
                                json!({ "text": format!(
                                "Compaction failed ({e}); continuing with full history."
                            ) }),
                                None,
                                None,
                                None,
                            )
                            .await;
                        }
                    }
                }
            }
            let system_value: Value = if deps.meta.supports_prompt_cache {
                json!([{ "type": "text", "text": system, "cache_control": {"type": "ephemeral"} }])
            } else {
                json!(system)
            };
            let mut body = json!({
                "model": model,
                "system": system_value,
                // `cm.messages_for_request()` applies the sanitized projection:
                // dangling tool_use ids from an interrupted prior turn get
                // synthesized error tool_results, or Anthropic 400s the whole
                // request (and the session stays poisoned).
                "messages": cm.messages_for_request(),
                "tools": tool_defs,
                "max_tokens": max_tokens,
                "stream": true,
            });
            if let Some(budget) = thinking_budget {
                body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
            }

            let mut rx = match deps.llm.stream(body).await {
                Ok(rx) => rx,
                Err(e) if is_context_overflow(&e.to_string()) => {
                    cm.mark_full();
                    // Display-only: `mark_full` does not reset `cm.last_*`, so
                    // they still hold the PREVIOUS committed response's buckets
                    // — accumulating here would double-count it.
                    emit_context_display(deps, cm, display.text()).await;
                    anyhow::bail!(
                        "context window exceeded — send another message and the session \
                     will compact before retrying: {e}"
                    );
                }
                Err(e) => return Err(e),
            };
            let mut turn = TurnAccum::default();
            let mut text_buf = String::new();

            while let Some(item) = rx.recv().await {
                if cancel.is_cancelled() {
                    // Mid-stream cancel: the assistant turn was not appended, so the
                    // ledger still ends at the user turn — valid for a later resume.
                    return Ok(final_text);
                }
                let ev = match item {
                    Ok(ev) => ev,
                    Err(e) => {
                        flush_text(deps, &mut text_buf, display.text()).await;
                        if is_context_overflow(&e.to_string()) {
                            cm.mark_full();
                            // Display-only — see the comment on the `deps.llm.stream` overflow arm above.
                            emit_context_display(deps, cm, display.text()).await;
                            anyhow::bail!(
                                "context window exceeded — send another message and the session \
                             will compact before retrying: {e}"
                            );
                        }
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
                            flush_text(deps, &mut text_buf, display.text()).await;
                        }
                    }
                    MessageStreamEvent::ThinkingDelta { text, .. } => {
                        if display.text() {
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
                    MessageStreamEvent::MessageDelta {
                        stop_reason,
                        output_tokens,
                        input_tokens,
                        cache_read_tokens,
                        cache_creation_tokens,
                    } => {
                        turn.stop_reason = stop_reason;
                        cm.observe_message_delta(
                            output_tokens,
                            input_tokens,
                            cache_read_tokens,
                            cache_creation_tokens,
                        );
                    }
                    MessageStreamEvent::Error(msg) => {
                        flush_text(deps, &mut text_buf, display.text()).await;
                        if is_context_overflow(&msg) {
                            cm.mark_full();
                            // Display-only — see the comment on the `deps.llm.stream` overflow arm above.
                            emit_context_display(deps, cm, display.text()).await;
                            anyhow::bail!(
                                "context window exceeded — send another message and the session \
                             will compact before retrying: {msg}"
                            );
                        }
                        anyhow::bail!("{msg}");
                    }
                    MessageStreamEvent::MessageStop => break,
                    MessageStreamEvent::MessageStart(msg) => {
                        cm.observe_message_start(&msg);
                    }
                    MessageStreamEvent::ContentBlockStop { .. } => {}
                }
            }
            flush_text(deps, &mut text_buf, display.text()).await;
            cm.commit_response();
            emit_context_usage(deps, cm, display.text()).await;
            if !turn.text.is_empty() {
                final_text = turn.text.clone();
            }

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
            if content.is_empty() {
                // An assistant turn must exist for valid role alternation, but an
                // EMPTY text block ({"text":""}) makes Anthropic 400 the NEXT
                // request ("text content blocks must be non-empty") — which
                // poisons the whole session. Use a non-empty sentinel instead.
                content.push(json!({ "type": "text", "text": "(no output)" }));
            }
            cm.append_assistant(json!(content)).await?;

            if tool_calls.is_empty() {
                // The model answered in plain text with no tool call — normally
                // end_turn. But a steer that landed during this round must not be
                // dropped: the only other drain site rides the tool-result batch
                // below, which this branch never reaches. Drain it as a user
                // message and loop once more so the model actually responds to the
                // steer, instead of losing it — or leaking it, stale, into a later
                // unrelated turn's tool-result batch.
                if let Some(block) = deps.steer.take_block() {
                    cm.append_user_text(&block).await?;
                    provider_turn += 1;
                    continue;
                }
                return Ok(final_text); // end_turn
            }

            // Execute each tool call, collecting tool_result blocks.
            let mut results: Vec<Value> = Vec::new();
            for (i, t) in tool_calls.iter().enumerate() {
                if cancel.is_cancelled() {
                    for rest in &tool_calls[i..] {
                        results.push(tool_result(&rest.id, "Interrupted by user", true));
                    }
                    break;
                }
                results.push(run_tool_call(deps, agent, t, &display, &spawn, cancel).await);
            }
            cm.append_tool_results(results).await?;

            // Mid-turn steering (Task B3): a message sent while this turn was
            // running is queued in `deps.steer`, not raced into the ledger
            // directly. Drain it now — right after the tool-result batch it rides
            // alongside — so the model sees it on the NEXT iteration's request,
            // wrapped in the verbatim marker the system prompt teaches it to
            // trust as a direct user instruction.
            if let Some(block) = deps.steer.take_block() {
                cm.append_user_text(&block).await?;
            }

            if cancel.is_cancelled() {
                return Ok(final_text);
            }
            provider_turn += 1;
        }
        // Budget window exhausted without an end_turn. Auto-continue (#100) is a
        // top-level convenience only (sub-agents have auto_budget == 0, so this
        // never fires for them): tell the user, append a synthetic "continue"
        // user turn to the ledger (ledger-only — NOT a display row, so the
        // transcript shows the notice, not a fake user message), re-grant a
        // fresh budget window, and loop back into `while budget.try_consume()`.
        // Guarded by `!cancel.is_cancelled()`: if the user stopped the run right
        // as the window exhausted, we must not announce an auto-continue or
        // append a synthetic turn the run will never act on.
        if auto_continue < auto_budget && !cancel.is_cancelled() {
            if display.text() {
                emit_row(
                    deps,
                    "system",
                    "notice",
                    json!({ "text": format!(
                        "Turn limit reached ({max_turns} provider turns) — continuing automatically ({}/{auto_budget})…",
                        auto_continue + 1
                    ) }),
                    None,
                    None,
                    None,
                )
                .await;
            }
            cm.append_user(json!([{ "type": "text", "text": "continue" }]))
                .await?;
            // Re-grant a fresh window so the budget loop resumes; refund()
            // restores one iteration at a time, so grant a full window's worth.
            for _ in 0..max_turns {
                budget.refund();
            }
            auto_continue += 1;
            continue;
        }
        // Auto-continue spent (or disabled): fall through to the budget-exhausted
        // summary tail below.
        break;
    }
    // A steer that landed after the loop's last drain — or while the final
    // tool round was still pending when the budget ran out — is still buffered.
    // Fold it into `cm` so the terminal summary call (and, when no model is
    // configured, the ledger the next turn resumes from) sees it rather than
    // dropping it silently.
    if let Some(block) = deps.steer.take_block() {
        cm.append_user_text(&block).await?;
    }
    // Budget exhausted with tool calls still pending: make one tool-less
    // call asking the model for a final summary instead of leaving the user
    // with a bare notice (Hermes' pattern). The nudge + summary are only
    // committed to `cm` once the call actually succeeds — a failed or empty
    // call leaves history exactly as the loop left it and falls through to
    // the notice below, so a botched aux call can't poison the session with
    // an unanswered nudge.
    if !model.is_empty() {
        let mut messages = cm.messages_for_request();
        messages.push(json!({
            "role": "user",
            "content": [{ "type": "text", "text": BUDGET_EXHAUSTED_PROMPT }],
        }));
        let body = json!({
            "model": model,
            "system": system,
            "messages": messages,
            "max_tokens": max_tokens,
            "stream": true,
        });
        if let Ok(text) = super::llm::collect_text(&deps.llm, body).await {
            let text = text.trim();
            if !text.is_empty() {
                let text = text.to_string();
                cm.append_user_text(BUDGET_EXHAUSTED_PROMPT).await?;
                cm.append_assistant_text(&text).await?;
                if display.text() {
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
                return Ok(text);
            }
        }
    }
    // Fallback: no model configured, the summary call errored, or it
    // returned nothing — keep the original bare notice.
    if display.text() {
        emit_row(
            deps,
            "system",
            "notice",
            json!({ "text": format!(
                "Turn limit reached ({provider_turn} provider turns) — send a message to continue."
            ) }),
            None,
            None,
            None,
        )
        .await;
    }
    Ok(final_text)
}

/// Effort → extended-thinking budget (spec §8): low/None → off, medium → 8192,
/// high → 16384, clamped to half of max_tokens; only for reasoning models.
///
/// This key currently only takes effect on OpenAI-format upstreams: the
/// router (`llm_router::client::anthropic_messages_stream`) strips
/// `thinking` before it reaches an Anthropic-native upstream (passthrough
/// `/messages` and kiro), since the runner doesn't yet replay signed
/// thinking blocks in tool-use continuations and newest Anthropic models
/// reject `budget_tokens` outright. The OpenAI translator maps this key to
/// `reasoning_effort` instead, so it's unaffected.
fn thinking_budget(
    effort: Option<&str>,
    meta: &crate::llm_router::model_meta::ModelMeta,
    max_tokens: i64,
) -> Option<i64> {
    if !meta.supports_reasoning {
        return None;
    }
    let budget = match effort {
        Some("medium") => 8_192,
        Some("high") | Some("xhigh") | Some("max") => 16_384,
        _ => return None,
    };
    Some(budget.min(max_tokens / 2))
}

/// Tools delegated children may never use regardless of filters. `task` is
/// re-armed for delegator agents (the orchestrator role); `memory` never is —
/// sub-agents run memoryless, mirroring hermes-agent's `skip_memory`. The todo
/// tools are blocked because the list is keyed by the parent's session_pk: a
/// child's `todowrite` would silently clobber the user-visible plan.
const SUBAGENT_BLOCKLIST: &[&str] = &["task", "memory", "todowrite", "todoread"];
/// Cap on one delegated child's model-visible report (protects the parent's
/// context from runaway child output).
const MAX_SUBTASK_REPORT_CHARS: usize = 16_000;

/// Truncate an oversized child report, keeping 75% head / 25% tail.
fn cap_report(s: &str) -> String {
    let n = s.chars().count();
    if n <= MAX_SUBTASK_REPORT_CHARS {
        return s.to_string();
    }
    let head_n = MAX_SUBTASK_REPORT_CHARS * 3 / 4;
    let tail_n = MAX_SUBTASK_REPORT_CHARS - head_n;
    let head: String = s.chars().take(head_n).collect();
    let tail: String = s.chars().skip(n - tail_n).collect();
    format!("{head}\n[… {} chars elided …]\n{tail}", n - head_n - tail_n)
}

/// The tool filter a delegated child actually runs with: the intersection of
/// the parent's and the child agent's filters over the registry's tool names,
/// minus the delegation blocklist.
fn effective_child_filter(
    parent: &super::agents::ToolFilter,
    child: &super::agents::ToolFilter,
    names: &[String],
    blocklist: &[&str],
) -> super::agents::ToolFilter {
    super::agents::ToolFilter::Only(
        names
            .iter()
            .filter(|n| parent.allows(n) && child.allows(n) && !blocklist.contains(&n.as_str()))
            .cloned()
            .collect(),
    )
}

/// The `max_spawn_depth` setting (default 2: a delegating sub-agent like the
/// builtin `orchestrator` can itself delegate one level; its children cannot).
async fn max_spawn_depth(store: &Store) -> u8 {
    crate::settings::usize_setting(store, "max_spawn_depth", 2).await as u8
}

/// A [`SubagentSpawner`] backed by the runner: runs sub-agents in ephemeral
/// (unpersisted-history) sub-loops and returns their final texts. `depth` is
/// how many delegation hops separate this spawner from the primary agent.
struct RunnerSpawner {
    deps: RunnerDeps,
    cancel: CancellationToken,
    depth: u8,
}

impl RunnerSpawner {
    /// The `max_concurrent_runs` setting (default 3, floor 1).
    async fn concurrency(&self) -> usize {
        crate::settings::usize_setting(&self.deps.store, "max_concurrent_runs", 3).await
    }

    /// Run one delegated child to completion; failures become the result's
    /// status, never a panic or batch abort.
    async fn run_child(
        &self,
        index: usize,
        spec: SubtaskSpec,
        cancel: CancellationToken,
    ) -> SubtaskResult {
        let result = |status, report| SubtaskResult {
            index,
            agent_type: spec.agent_type.clone(),
            status,
            report,
        };
        let Some(agent) = self
            .deps
            .agents
            .get(&spec.agent_type)
            .filter(|a| a.mode.is_subagent())
        else {
            return result(
                SubtaskStatus::Error,
                format!(
                    "unknown sub-agent `{}` (available: {})",
                    spec.agent_type,
                    self.available().join(", ")
                ),
            );
        };
        let mut child = agent;
        child.tools = effective_child_filter(
            &self.deps.agent.tools,
            &child.tools,
            &self.deps.tools.names(),
            SUBAGENT_BLOCKLIST,
        );
        // Orchestrator role: a delegating child gets the `task` tool re-armed
        // and a spawner one hop deeper, while the spawn-depth budget allows.
        let child_depth = self.depth.saturating_add(1);
        let max_depth = max_spawn_depth(&self.deps.store).await;
        let delegates = child.can_delegate && child_depth < max_depth;
        if delegates {
            if let super::agents::ToolFilter::Only(list) = &mut child.tools {
                if !list.iter().any(|t| t == "task") {
                    list.push("task".to_string());
                }
            }
            let block = format!(
                "\n\nYou may delegate subtasks with the `task` tool (spawn depth \
                 {child_depth} of {max_depth}). Delegate only self-contained work — \
                 sub-agents cannot see your conversation. Prefer the batch form for \
                 independent subtasks; do small work yourself."
            );
            child.prompt = Some(match child.prompt.take() {
                Some(p) => format!("{p}{block}"),
                None => format!(
                    "{}{block}",
                    context::assemble_system(
                        &self.deps.work_dir,
                        &self.deps.extra_skill_dirs,
                        None
                    )
                ),
            });
        }
        // Tool rows only (tagged with the sub-agent label), no memory access;
        // history is ephemeral.
        let mut child_deps = self.deps.clone();
        child_deps.memory = None;
        child_deps.agent = child.clone();
        let child_spawn: Option<Arc<dyn SubagentSpawner>> = if delegates {
            Some(Arc::new(RunnerSpawner {
                deps: child_deps.clone(),
                cancel: cancel.clone(),
                depth: child_depth,
            }))
        } else {
            None
        };
        let mut cm = ContextManager::ephemeral(
            &self.deps.session_pk,
            ContextConfig::with_meta(self.deps.meta),
        );
        if let Err(e) = cm
            .append_user(json!([{ "type": "text", "text": spec.prompt }]))
            .await
        {
            return result(SubtaskStatus::Error, e.to_string());
        }
        let display = DisplayMode::ToolsOnly {
            label: spec.agent_type.clone(),
        };
        let child_budget = IterationBudget::new(SUBAGENT_MAX_ITERS);
        match drive(
            &child_deps,
            &child,
            &mut cm,
            &cancel,
            child_spawn,
            display,
            &child_budget,
        )
        .await
        {
            Ok(text) if cancel.is_cancelled() => {
                result(SubtaskStatus::Interrupted, cap_report(&text))
            }
            Ok(text) => result(SubtaskStatus::Completed, cap_report(&text)),
            Err(e) => result(SubtaskStatus::Error, e.to_string()),
        }
    }
}

#[async_trait]
impl SubagentSpawner for RunnerSpawner {
    async fn run_many(&self, specs: Vec<SubtaskSpec>) -> Vec<SubtaskResult> {
        let sem = Arc::new(tokio::sync::Semaphore::new(self.concurrency().await));
        let futures = specs.into_iter().enumerate().map(|(index, spec)| {
            let sem = sem.clone();
            let cancel = self.cancel.child_token();
            async move {
                let _permit = sem.acquire().await;
                if cancel.is_cancelled() {
                    return SubtaskResult {
                        index,
                        agent_type: spec.agent_type,
                        status: SubtaskStatus::Interrupted,
                        report: "interrupted before start".into(),
                    };
                }
                self.run_child(index, spec, cancel).await
            }
        });
        let mut results = futures::future::join_all(futures).await;
        results.sort_by_key(|r| r.index);
        results
    }

    fn available(&self) -> Vec<String> {
        self.deps
            .agents
            .subagents()
            .into_iter()
            .map(|a| a.name)
            .collect()
    }
}

/// Insert the tool_call row (if displaying), gate it, execute, and update the
/// row. Returns the Anthropic `tool_result` block to append to the ledger.
async fn run_tool_call(
    deps: &RunnerDeps,
    agent: &Agent,
    t: &ToolAccum,
    display: &DisplayMode,
    spawn: &Option<Arc<dyn SubagentSpawner>>,
    cancel: &CancellationToken,
) -> Value {
    let input = t.parsed_input();
    let Some(tool) = deps.tools.get(&t.name) else {
        let msg = format!("unknown tool `{}`", t.name);
        insert_tool_row(deps, t, &input, "unknown", display.subagent()).await;
        finish_tool_row(deps, &t.id, &msg, true).await;
        return tool_result(&t.id, &msg, true);
    };
    // Enforce the agent's tool allow-list.
    if !agent.tools.allows(&t.name) {
        let msg = format!(
            "tool `{}` is not permitted for the `{}` agent",
            t.name, agent.name
        );
        insert_tool_row(deps, t, &input, tool.kind(), display.subagent()).await;
        finish_tool_row(deps, &t.id, &msg, true).await;
        return tool_result(&t.id, &msg, true);
    }
    insert_tool_row(deps, t, &input, tool.kind(), display.subagent()).await;

    // Plugin hooks: a `tool.before` hook may deny the call.
    let hook = super::hooks::run(
        &deps.work_dir,
        "tool.before",
        &json!({ "tool": t.name, "input": input }),
    )
    .await;
    if !hook.allowed {
        let msg = hook
            .message
            .unwrap_or_else(|| "blocked by plugin hook".to_string());
        finish_tool_row(deps, &t.id, &msg, true).await;
        return tool_result(&t.id, &msg, true);
    }

    // Permission gate. Read the mode fresh so a mid-session change applies.
    let perm_mode = deps.current_perm_mode();
    let spec = tool.permission(&input);
    let gate = super::permission::PermGate {
        perm_mode,
        project_id: deps.project_id.as_deref(),
        store: &deps.store,
        overrides: &deps.perm_overrides,
        session_pk: &deps.session_pk,
        tool_call_id: &t.id,
        approvals: &deps.approvals,
        events: &deps.events,
        cancel,
    };
    let decision = evaluate(&spec, &input, &gate).await;
    if decision == PermDecision::Deny {
        // Plan mode denies mutations outright (no prompt) — tell the model why
        // so it plans instead of retrying, rather than showing "Denied by user".
        let msg = if cancel.is_cancelled() {
            // Stopped while gated/parked: pair the tool_use with an
            // interrupted tool_result, not a user denial.
            "Interrupted by user"
        } else if perm_mode == PermMode::Plan && !matches!(tool.kind(), "read") {
            "Plan mode is read-only: file edits and shell commands are disabled. \
             Propose a plan for the user to review; they can switch to Ask/Edit/Full to execute it."
        } else {
            "Denied by user"
        };
        finish_tool_row(deps, &t.id, msg, true).await;
        return tool_result(&t.id, msg, true);
    }

    // Snapshot the worktree before a mutating tool runs, so `revert` can undo
    // it. `revert` itself must not snapshot (it would capture the change it is
    // about to undo).
    if matches!(tool.kind(), "edit" | "execute") && t.name != "revert" {
        if let Some(sha) = super::snapshot::take(&deps.work_dir).await {
            deps.snapshots.lock().await.push(sha);
        }
    }

    // Execute. Timed from here — after the permission gate — so a long human
    // approval wait never inflates the card's duration badge.
    let started = std::time::Instant::now();
    let ctx = ToolCtx {
        session_pk: deps.session_pk.clone(),
        work_dir: deps.work_dir.clone(),
        attachments_dir: deps.attachments_dir.clone(),
        extra_skill_dirs: deps.extra_skill_dirs.clone(),
        store: deps.store.clone(),
        cancel: cancel.clone(),
        caps: OutputCaps::default(),
        spawn: spawn.clone(),
        memory: deps.memory.clone(),
        snapshots: deps.snapshots.clone(),
        tool_call_id: t.id.clone(),
        interaction: Some(Arc::new(super::tools::Interaction {
            approvals: deps.approvals.clone(),
            events: deps.events.clone(),
            perm_mode: deps.perm_mode.clone(),
            project_id: deps.project_id.clone(),
        })),
    };
    match tool.execute(&ctx, input).await {
        Ok(mut out) => {
            let extras = merge_display_duration(out.display.take(), elapsed_ms(started));
            finish_tool_row_with_display(deps, &t.id, &out.for_model, out.is_error, Some(extras))
                .await;
            match out.model_blocks.take() {
                Some(mut blocks) => {
                    blocks.push(json!({ "type": "text", "text": out.for_model }));
                    json!({
                        "type": "tool_result",
                        "tool_use_id": t.id,
                        "content": blocks,
                        "is_error": out.is_error,
                    })
                }
                None => tool_result(&t.id, &out.for_model, out.is_error),
            }
        }
        Err(e) => {
            let msg = format!("{}: {e}", t.name);
            let extras = merge_display_duration(None, elapsed_ms(started));
            finish_tool_row_with_display(deps, &t.id, &msg, true, Some(extras)).await;
            tool_result(&t.id, &msg, true)
        }
    }
}

/// Insert the initial `tool_call` row (`{name, input}`, in_progress).
async fn insert_tool_row(
    deps: &RunnerDeps,
    t: &ToolAccum,
    input: &Value,
    kind: &str,
    subagent: Option<&str>,
) {
    let mut payload = json!({ "name": t.name, "input": input });
    if let Some(label) = subagent {
        payload["subagent"] = json!(label);
    }
    emit_row(
        deps,
        "assistant",
        "tool_call",
        payload,
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

/// Milliseconds elapsed since `started`, saturating into a JSON-safe u64.
fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Fold the measured duration into a tool's display extras (`{"summary": …}`,
/// `{"exit_code": …}`, …). The result is json_patch-merged into the persisted
/// tool_call payload by [`finish_tool_row_with_display`], so `duration_ms`
/// and the other extras survive session hydration. Non-object extras are
/// discarded — a scalar would corrupt the payload merge.
fn merge_display_duration(display: Option<Value>, duration_ms: u64) -> Value {
    let mut extras = match display {
        Some(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    extras.insert("duration_ms".to_string(), Value::from(duration_ms));
    Value::Object(extras)
}

/// Flush any buffered streaming text as one delta-shaped `text` row (only when
/// displaying — sub-agents keep their text internal).
async fn flush_text(deps: &RunnerDeps, buf: &mut String, emit_display: bool) {
    if buf.is_empty() {
        return;
    }
    let text = std::mem::take(buf);
    if emit_display {
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

/// Broadcast ContextUsage and persist it for resume seeding. Sub-agent
/// (ephemeral) loops skip both — their usage must not clobber the session's.
/// Also folds this response's billed buckets into the per-session, per-model
/// cost tally and emits `SessionCost` alongside `ContextUsage`.
///
/// Call this ONLY immediately after a fresh `cm.commit_response()` — it is
/// the single site allowed to accumulate, because `cm.last_input()` /
/// `last_output()` / `last_cache_read()` / `last_cache_creation()` hold
/// exactly the response that was just committed there and nowhere else.
/// Every other `ContextUsage` re-emit (context-overflow `mark_full`, manual
/// `/compact`, the pre-turn resume/fallback seed) reads those same stale
/// accessors from a PREVIOUS commit and must go through
/// [`emit_context_display`] instead, or that response's buckets get added to
/// the tally a second time.
async fn emit_context_usage(deps: &RunnerDeps, cm: &ContextManager, emit: bool) {
    if !emit {
        return;
    }
    let st = cm.status();
    let _ = deps.events.send(CoreEvent::ContextUsage {
        session_pk: deps.session_pk.clone(),
        active_tokens: st.active_tokens,
        context_window: st.context_window,
        usable_window: st.usable_window,
        percent_left: st.percent_left,
        cache_read_tokens: cm.last_cache_read(),
        output_tokens: cm.last_output(),
    });

    // Accumulate this response's billed buckets into the per-model tally, then
    // emit the session cost. Read-modify-write is race-free: native turns are
    // serialized by the session turn_lock.
    let saved = match deps.store.get_session_context(&deps.session_pk).await {
        Ok(saved) => saved,
        Err(e) => {
            // A transient read error must never be treated as "no tally yet"
            // — that would drop everything accumulated so far the moment we
            // write back. Skip this emit's accumulation/persist entirely and
            // let the next successful read pick the tally back up.
            tracing::warn!(
                "native: get_session_context failed, skipping cost accumulation to avoid \
                 clobbering the persisted tally: {e}"
            );
            return;
        }
    };
    let mut tally = saved
        .as_ref()
        .map(super::cost::Tally::from_payload)
        .unwrap_or_default();
    tally.add(
        deps.model.as_deref().unwrap_or("unknown"),
        cm.last_input(),
        cm.last_output(),
        cm.last_cache_read(),
        cm.last_cache_creation(),
    );
    emit_session_cost(deps, &tally).await;

    let payload = json!({
        "active_tokens": st.active_tokens,
        "usable_window": st.usable_window,
        "percent_left": st.percent_left,
        "models": tally.to_payload_value(),
    });
    if let Err(e) = deps
        .store
        .upsert_session_context(&deps.session_pk, &payload)
        .await
    {
        tracing::warn!("native: upsert_session_context failed: {e}");
    }
}

/// Display-only `ContextUsage` re-emit, for every site that is NOT
/// immediately after a fresh `cm.commit_response()`: the context-overflow
/// `mark_full` sites, manual `/compact`, and the pre-turn resume/fallback
/// seed. `cm.last_*` at those sites still hold whatever the last real
/// committed response left behind (`mark_full` and `compact()` never reset
/// them), so this function never calls `Tally::add` — it only re-broadcasts
/// the tally exactly as already persisted (if any) and refreshes the context
/// snapshot fields (`active_tokens`/`usable_window`/`percent_left`), leaving
/// `"models"` byte-for-byte untouched.
async fn emit_context_display(deps: &RunnerDeps, cm: &ContextManager, emit: bool) {
    if !emit {
        return;
    }
    let st = cm.status();
    let _ = deps.events.send(CoreEvent::ContextUsage {
        session_pk: deps.session_pk.clone(),
        active_tokens: st.active_tokens,
        context_window: st.context_window,
        usable_window: st.usable_window,
        percent_left: st.percent_left,
        cache_read_tokens: cm.last_cache_read(),
        output_tokens: cm.last_output(),
    });

    let saved = match deps.store.get_session_context(&deps.session_pk).await {
        Ok(saved) => saved,
        Err(e) => {
            // Same clobber hazard as `emit_context_usage`: without a good
            // read we don't know what's already persisted, so skip the
            // persist for this emit rather than writing a models-less
            // payload over a real tally.
            tracing::warn!(
                "native: get_session_context failed, skipping context-display persist to avoid \
                 clobbering the persisted tally: {e}"
            );
            return;
        }
    };
    // `Ok(None)` (genuinely no tally yet) is a legitimate empty base.
    let tally = saved
        .as_ref()
        .map(super::cost::Tally::from_payload)
        .unwrap_or_default();
    // Keep the UI in sync with the resume block: re-emit from the UNCHANGED
    // saved tally when there's something to show — no accumulation, just
    // pricing it at current rates like the resume re-emit does.
    if !tally.is_empty() {
        emit_session_cost(deps, &tally).await;
    }

    let payload = json!({
        "active_tokens": st.active_tokens,
        "usable_window": st.usable_window,
        "percent_left": st.percent_left,
        "models": tally.to_payload_value(),
    });
    if let Err(e) = deps
        .store
        .upsert_session_context(&deps.session_pk, &payload)
        .await
    {
        tracing::warn!("native: upsert_session_context failed: {e}");
    }
}

/// Price a tally against the current model metadata and broadcast SessionCost.
async fn emit_session_cost(deps: &RunnerDeps, tally: &super::cost::Tally) {
    // Resolve each model's meta once, up front (async), into a map the pure
    // pricer closes over.
    let mut metas: std::collections::HashMap<String, crate::llm_router::model_meta::ModelMeta> =
        std::collections::HashMap::new();
    for model in tally.model_ids() {
        let meta = crate::llm_router::model_meta::resolve(&deps.store, &model).await;
        metas.insert(model, meta);
    }
    let (total_usd, models) = tally.to_model_costs(|id| {
        metas
            .get(id)
            .copied()
            .unwrap_or(crate::llm_router::model_meta::FALLBACK)
    });
    let _ = deps.events.send(CoreEvent::SessionCost {
        session_pk: deps.session_pk.clone(),
        total_usd,
        models,
    });
}

/// Sub-agent (ephemeral) compactions must never surface to the parent
/// session: they operate on the child's own throwaway ledger, so neither the
/// `ContextCompacted` event nor the notice row is the session's business —
/// gate the whole function on `emit_display`, matching `emit_context_usage`.
async fn emit_compaction(
    deps: &RunnerDeps,
    trigger: &str,
    outcome: &CompactionOutcome,
    emit_display: bool,
) {
    if !emit_display {
        return;
    }
    let _ = deps.events.send(CoreEvent::ContextCompacted {
        session_pk: deps.session_pk.clone(),
        trigger: trigger.to_string(),
        before_tokens: outcome.before_tokens,
        after_tokens: outcome.after_tokens,
        window_number: outcome.window_number,
    });
    let text = format!(
        "Context compacted: ~{}k → ~{}k tokens",
        outcome.before_tokens / 1000,
        outcome.after_tokens / 1000
    );
    emit_row(
        deps,
        "system",
        "notice",
        json!({ "text": text }),
        None,
        None,
        None,
    )
    .await;
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

    /// Wraps [`ScriptedLlm`], recording every request body for assertions.
    pub struct RecordingLlm {
        inner: ScriptedLlm,
        pub bodies: Mutex<Vec<Value>>,
    }

    impl RecordingLlm {
        pub fn new(turns: Vec<Vec<AnthropicEvent>>) -> Self {
            RecordingLlm {
                inner: ScriptedLlm::new(turns),
                bodies: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LlmStream for RecordingLlm {
        async fn stream(
            &self,
            body: Value,
        ) -> anyhow::Result<mpsc::Receiver<anyhow::Result<AnthropicEvent>>> {
            self.bodies.lock().unwrap().push(body.clone());
            self.inner.stream(body).await
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
    /// message_start carrying Anthropic-style usage.
    pub fn message_start_with_usage(input: i64, cache_read: i64) -> AnthropicEvent {
        (
            "message_start".into(),
            serde_json::json!({
                "type": "message_start",
                "message": {"id": "m1", "role": "assistant", "content": [],
                             "usage": {"input_tokens": input,
                                       "cache_read_input_tokens": cache_read}}
            }),
        )
    }
    pub fn error_event(message: &str) -> AnthropicEvent {
        (
            "error".into(),
            serde_json::json!({"type": "error", "error": {"message": message}}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::ledger::Ledger;
    use super::testutil::*;
    use super::*;
    use crate::domain::CoreEvent;
    use crate::store::Store;

    #[test]
    fn display_mode_gating() {
        let full = DisplayMode::Full;
        let sub = DisplayMode::ToolsOnly {
            label: "explore".into(),
        };
        assert!(full.text() && full.subagent().is_none());
        assert!(!sub.text());
        assert_eq!(sub.subagent(), Some("explore"));
    }

    async fn deps_at(dir: &std::path::Path, llm: Arc<dyn LlmStream>) -> RunnerDeps {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());
        let (events, _rx) = broadcast::channel(256);
        let agents = Arc::new(AgentRegistry::builtin());
        let agent = agents.default_agent();
        RunnerDeps {
            session_pk: "s1".into(),
            work_dir: dir.to_path_buf(),
            attachments_dir: None,
            extra_skill_dirs: vec![],
            // bypassPermissions so the scripted bash tool runs without a prompt.
            model: Some("test/model".into()),
            effort: None,
            meta: crate::llm_router::model_meta::FALLBACK,
            perm_mode: Arc::new(std::sync::Mutex::new(PermMode::BypassPermissions)),
            project_id: None,
            perm_overrides: Arc::new(std::sync::Mutex::new(Default::default())),
            store,
            events,
            approvals: Arc::new(ApprovalHub::new()),
            llm,
            tools: Arc::new(ToolRegistry::builtin()),
            agent,
            agents,
            commands: Arc::new(CommandRegistry::builtin()),
            memory: None,
            snapshots: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            steer: SteerBuffer::new(),
        }
    }

    /// Seed a project (pinned to `model`) plus a TITLED session "s1" so the
    /// per-turn snapshot has rows to read while title generation stays off
    /// (an untitled session row would consume an extra scripted LLM turn).
    async fn seed_pinned_project(store: &Store, model: Option<&str>) {
        use crate::domain::{Project, Session, SessionKind, SessionStatus};
        store
            .insert_project(Project {
                project_id: "p".into(),
                name: "p".into(),
                workdir: "/w".into(),
                source: None,
                harness: "native".into(),
                model: model.map(str::to_string),
                effort: None,
                perm_mode: PermMode::BypassPermissions,
                created_at: Some(0),
                is_git: false,
            })
            .await
            .unwrap();
        store
            .insert_session(Session {
                session_pk: "s1".into(),
                project_id: Some("p".into()),
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: Some("titled".into()),
                status: SessionStatus::Running,
                perm_mode: PermMode::BypassPermissions,
                started_by: None,
                created_at: Some(0),
                last_active: Some(0),
                resume_attempts: 0,
                branch_owned: true,
                kind: SessionKind::Project,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
    }

    /// An enabled anthropic API-key connection serving exactly `models`, so
    /// `family/model` pins like "anthropic/model-a" resolve through routing.
    async fn add_anthropic_conn(store: &Store, models: &[&str]) {
        use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};
        connections::add_connection(
            store,
            ConnectionRow {
                id: "claude".into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: "claude".into(),
                priority: 0,
                enabled: true,
                data: ConnectionData {
                    api_key: Some("sk-test".into()),
                    models_override: Some(models.iter().map(|m| m.to_string()).collect()),
                    ..Default::default()
                },
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn next_turn_picks_up_a_mid_chat_model_change() {
        use testutil::RecordingLlm;
        let dir = tempfile::tempdir().unwrap();
        let turn1 = vec![text_delta("one"), message_delta("end_turn"), message_stop()];
        let turn2 = vec![text_delta("two"), message_delta("end_turn"), message_stop()];
        let llm = Arc::new(RecordingLlm::new(vec![turn1, turn2]));
        let mut deps = deps_at(dir.path(), llm.clone()).await;
        // Simulate what start_session froze into RunnerDeps at session start.
        deps.model = Some("anthropic/model-a".into());
        add_anthropic_conn(&deps.store, &["model-a", "model-b"]).await;
        seed_pinned_project(&deps.store, Some("anthropic/model-a")).await;

        run_turn(
            &deps,
            TurnPrompt::text("one", "one"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        // The user repins the model mid-chat (exactly what the composer's
        // model picker writes via update_project).
        deps.store
            .update_project(
                "p",
                Some("anthropic/model-b".into()),
                PermMode::BypassPermissions,
                "native",
            )
            .await
            .unwrap();

        run_turn(
            &deps,
            TurnPrompt::text("two", "two"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        {
            let bodies = llm.bodies.lock().unwrap();
            assert_eq!(bodies.len(), 2);
            assert_eq!(bodies[0]["model"], "anthropic/model-a");
            assert_eq!(
                bodies[1]["model"], "anthropic/model-b",
                "the next turn must re-read the project's pinned model"
            );
        }

        // Negative invariant: both pins (model-a, then model-b) are routable
        // via the connection seeded above, so neither turn should have
        // announced a substitution.
        let msgs = deps.store.list_messages("s1").await.unwrap();
        assert!(
            !msgs.iter().any(|m| m.payload["summary"]
                .as_str()
                .is_some_and(|s| s.contains("is not routable"))),
            "a routable pin must never emit a not-routable status row"
        );
    }

    #[tokio::test]
    async fn unroutable_pinned_model_surfaces_a_status_row() {
        use crate::llm_router::routes::{
            self, ModelRouteInfo, ModelRouteStrategy, ModelRouteTarget,
        };
        use testutil::RecordingLlm;
        let dir = tempfile::tempdir().unwrap();
        let turn = vec![text_delta("ok"), message_delta("end_turn"), message_stop()];
        let llm = Arc::new(RecordingLlm::new(vec![turn]));
        let deps = deps_at(dir.path(), llm.clone()).await;
        add_anthropic_conn(&deps.store, &["model-a"]).await;
        // A route the default-model fallback resolves to (mirrors
        // native/mod.rs::native_model_resolution_falls_back_from_an_unresolvable_model).
        routes::save_model_route(
            &deps.store,
            ModelRouteInfo {
                id: "r1".into(),
                name: "fable".into(),
                enabled: true,
                strategy: ModelRouteStrategy::Fallback,
                targets: vec![ModelRouteTarget {
                    provider: "anthropic".into(),
                    model: "model-a".into(),
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
        // The project pins a model no connection serves.
        seed_pinned_project(&deps.store, Some("anthropic/ghost")).await;

        run_turn(
            &deps,
            TurnPrompt::text("go", "go"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        // The request really used the substitute...
        assert_eq!(llm.bodies.lock().unwrap()[0]["model"], "fable");
        // ...and the substitution is announced instead of silent.
        let msgs = deps.store.list_messages("s1").await.unwrap();
        let status = msgs
            .iter()
            .find(|m| m.role == "system" && m.block_type == "status")
            .expect("a status transcript row");
        assert_eq!(
            status.payload["summary"],
            "model `anthropic/ghost` is not routable, using `fable`"
        );
        // It lands after the user's message, where the turn it affects starts.
        let user_seq = msgs.iter().find(|m| m.role == "user").unwrap().seq;
        assert!(status.seq > user_seq);
    }

    fn tiny_meta() -> crate::llm_router::model_meta::ModelMeta {
        crate::llm_router::model_meta::ModelMeta {
            context_window: 400, // tiny: threshold 360, usable 380
            max_output_tokens: 8_192,
            supports_prompt_cache: false,
            supports_reasoning: false,
            cost_input: 0.0,
            cost_output: 0.0,
            cost_cache_read: 0.0,
            cost_cache_write: 0.0,
        }
    }

    #[tokio::test]
    async fn usage_events_flow_into_context_usage_event_and_session_context() {
        let dir = tempfile::tempdir().unwrap();
        let turn = vec![
            message_start_with_usage(5_000, 1_000),
            text_delta("hi"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![turn]));
        let deps = deps_at(dir.path(), llm).await;
        let mut rx = deps.events.subscribe();
        run_turn(&deps, TurnPrompt::text("x", "x"), CancellationToken::new())
            .await
            .unwrap();
        // A ContextUsage event was emitted with server-truth numbers.
        let mut saw = None;
        while let Ok(ev) = rx.try_recv() {
            if let CoreEvent::ContextUsage {
                active_tokens,
                cache_read_tokens,
                ..
            } = ev
            {
                saw = Some((active_tokens, cache_read_tokens));
            }
        }
        let (active, cache) = saw.expect("a ContextUsage event");
        assert!(
            active >= 6_000,
            "input+cache+output committed, got {active}"
        );
        assert_eq!(cache, 1_000);
        // Persisted for resume seeding.
        let ctx = deps.store.get_session_context("s1").await.unwrap().unwrap();
        assert!(ctx["percent_left"].is_number());
    }

    /// Drain every `SessionCost` event currently queued on `rx`, returning the
    /// last one seen (mirrors how a real subscriber only cares about the
    /// latest snapshot).
    fn last_session_cost(
        rx: &mut broadcast::Receiver<CoreEvent>,
    ) -> Option<(f64, Vec<crate::domain::ModelCost>)> {
        let mut saw = None;
        while let Ok(ev) = rx.try_recv() {
            if let CoreEvent::SessionCost {
                total_usd, models, ..
            } = ev
            {
                saw = Some((total_usd, models));
            }
        }
        saw
    }

    #[tokio::test]
    async fn session_cost_accumulates_per_model_across_turns() {
        let dir = tempfile::tempdir().unwrap();
        let turn = || {
            vec![
                message_start_with_usage(5_000, 1_000),
                text_delta("hi"),
                message_delta("end_turn"),
                message_stop(),
            ]
        };
        let llm = Arc::new(ScriptedLlm::new(vec![turn(), turn()]));
        let deps = deps_at(dir.path(), llm).await;
        // "test/model" (deps_at's default) isn't in the vendored/refreshed
        // price snapshot, so `resolve` would otherwise fall back to FALLBACK's
        // $0 rates. Pin a settings override so the dollar total is checkable.
        deps.store
            .set_setting_raw(
                "models.meta.test/model",
                &json!({
                    "cost_input": 3.0,
                    "cost_output": 15.0,
                    "cost_cache_read": 1.5,
                    "cost_cache_write": 0.0
                })
                .to_string(),
            )
            .await
            .unwrap();
        let mut rx = deps.events.subscribe();

        run_turn(&deps, TurnPrompt::text("x", "x"), CancellationToken::new())
            .await
            .unwrap();

        let (total1, models1) =
            last_session_cost(&mut rx).expect("a SessionCost event after turn 1");
        assert_eq!(models1.len(), 1);
        assert_eq!(models1[0].model, "test/model");
        assert_eq!(models1[0].input, 5_000);
        assert_eq!(models1[0].output, 1);
        assert_eq!(models1[0].cache_read, 1_000);
        assert_eq!(models1[0].cache_creation, 0);
        // 3.0/1e6*5000 + 15.0/1e6*1 + 1.5/1e6*1000 == 0.016515
        assert!((total1 - 0.016515).abs() < 1e-9, "total1 {total1}");
        assert!((models1[0].usd - total1).abs() < 1e-9);

        run_turn(&deps, TurnPrompt::text("y", "y"), CancellationToken::new())
            .await
            .unwrap();

        // The SECOND turn's `emit_context_usage` accumulates on top of the
        // first (the session_context "models" tally persists across turns) —
        // note this also exercises the resume re-emit at the top of run_turn,
        // since `session_context` now already exists.
        let (total2, models2) =
            last_session_cost(&mut rx).expect("a SessionCost event after turn 2");
        assert_eq!(models2.len(), 1);
        assert_eq!(models2[0].input, 10_000);
        assert_eq!(models2[0].output, 2);
        assert_eq!(models2[0].cache_read, 2_000);
        assert!((total2 - total1 * 2.0).abs() < 1e-9, "total2 {total2}");

        // Persisted payload stores TOKENS only under "models" — never dollars.
        let ctx = deps.store.get_session_context("s1").await.unwrap().unwrap();
        let saved = &ctx["models"]["test/model"];
        assert_eq!(saved["input"], 10_000);
        assert_eq!(saved["output"], 2);
        assert_eq!(saved["cache_read"], 2_000);
        assert!(
            saved.get("usd").is_none(),
            "session_context must never persist dollars"
        );
    }

    #[tokio::test]
    async fn emit_context_usage_with_emit_false_does_not_accumulate_or_persist() {
        // Sub-agent (ephemeral) loops call `emit_context_usage(.., emit=false)`
        // — they must not accumulate into the session's cost tally or touch
        // `session_context` at all.
        let dir = tempfile::tempdir().unwrap();
        let llm: Arc<dyn LlmStream> = Arc::new(ScriptedLlm::new(vec![]));
        let deps = deps_at(dir.path(), llm).await;
        let cfg = ContextConfig::load(&deps.store, deps.meta).await;
        let mut cm = ContextManager::load(deps.store.clone(), &deps.session_pk, cfg)
            .await
            .unwrap();
        cm.observe_message_start(&json!({
            "usage": {"input_tokens": 999, "cache_read_input_tokens": 3}
        }));
        cm.observe_message_delta(7, None, None, None);
        cm.commit_response();

        let mut rx = deps.events.subscribe();
        emit_context_usage(&deps, &cm, false).await;

        assert!(
            rx.try_recv().is_err(),
            "emit=false must not send any event (ContextUsage or SessionCost)"
        );
        assert!(
            deps.store
                .get_session_context(&deps.session_pk)
                .await
                .unwrap()
                .is_none(),
            "emit=false must not write session_context"
        );
    }

    #[tokio::test]
    async fn overflow_display_reemit_does_not_double_count_committed_cost() {
        // Regression test for the commit-3c284b0 bug: `emit_context_usage`
        // used to be called from BOTH the post-commit site AND the
        // context-overflow `mark_full` re-emit sites, sharing the same
        // accumulation logic. `mark_full` never resets `cm.last_*`, so on
        // overflow those accessors still held the PREVIOUS committed
        // response's buckets — which then got added to the persisted tally
        // a SECOND time. This drives the REAL overflow path (a mid-stream
        // `MessageStreamEvent::Error` after a committed response) and
        // asserts the tally reflects that response's buckets exactly ONCE.
        let dir = tempfile::tempdir().unwrap();
        // Turn 1: commits a real response (buckets B: input 5_000, output 1,
        // cache_read 1_000) with a tool_use so the drive loop continues into
        // a second provider turn instead of returning.
        let turn1 = vec![
            message_start_with_usage(5_000, 1_000),
            text_delta("Working on it.\n"),
            tool_use_start(1, "call-1", "bash"),
            input_json_delta(1, "{\"command\":\"echo hi > out.txt\"}"),
            message_delta("tool_use"),
            message_stop(),
        ];
        // Turn 2: a mid-stream overflow error. This hits the
        // `MessageStreamEvent::Error` `mark_full` + display re-emit path
        // WITHOUT ever calling `cm.commit_response()` again, so `cm.last_*`
        // still hold turn 1's buckets when the display re-emit reads them.
        let turn2 = vec![error_event(
            "prompt is too long: 500000 tokens > 400000 maximum",
        )];
        let llm = Arc::new(ScriptedLlm::new(vec![turn1, turn2]));
        let deps = deps_at(dir.path(), llm).await;
        deps.store
            .set_setting_raw(
                "models.meta.test/model",
                &json!({
                    "cost_input": 3.0,
                    "cost_output": 15.0,
                    "cost_cache_read": 1.5,
                    "cost_cache_write": 0.0
                })
                .to_string(),
            )
            .await
            .unwrap();
        let mut rx = deps.events.subscribe();

        let err = run_turn(&deps, TurnPrompt::text("x", "x"), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("context"));

        // The overflow pinned the indicator to 0%, proving the display
        // re-emit did run (this isn't a no-op skip).
        let ctx = deps.store.get_session_context("s1").await.unwrap().unwrap();
        assert_eq!(ctx["percent_left"], 0);

        // The per-model tally must equal buckets B exactly ONCE, not 2×B:
        // input 5_000 (not 10_000), output 1 (not 2), cache_read 1_000 (not
        // 2_000).
        let saved = &ctx["models"]["test/model"];
        assert_eq!(saved["input"], 5_000, "input must not be double-counted");
        assert_eq!(saved["output"], 1, "output must not be double-counted");
        assert_eq!(
            saved["cache_read"], 1_000,
            "cache_read must not be double-counted"
        );
        assert_eq!(saved["cache_creation"], 0);

        // Same invariant on the broadcast side: the last `SessionCost` must
        // price buckets B once, not twice.
        let (total, models) = last_session_cost(&mut rx).expect("a SessionCost event");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].input, 5_000);
        assert_eq!(models[0].output, 1);
        assert_eq!(models[0].cache_read, 1_000);
        // 3.0/1e6*5000 + 15.0/1e6*1 + 1.5/1e6*1000 == 0.016515
        assert!((total - 0.016515).abs() < 1e-9, "total {total}");
    }

    #[tokio::test]
    async fn emit_context_display_after_commit_does_not_change_persisted_totals() {
        // Focused unit test (spec fallback tier): calling the display-only
        // function after a real accumulation must be a complete no-op on the
        // persisted tally and totals, even though it re-reads and re-writes
        // the context snapshot fields.
        let dir = tempfile::tempdir().unwrap();
        let llm: Arc<dyn LlmStream> = Arc::new(ScriptedLlm::new(vec![]));
        let deps = deps_at(dir.path(), llm).await;
        let cfg = ContextConfig::load(&deps.store, deps.meta).await;
        let mut cm = ContextManager::load(deps.store.clone(), &deps.session_pk, cfg)
            .await
            .unwrap();
        cm.observe_message_start(&json!({
            "usage": {"input_tokens": 999, "cache_read_input_tokens": 3}
        }));
        cm.observe_message_delta(7, None, None, None);
        cm.commit_response();

        // The one legitimate accumulation.
        emit_context_usage(&deps, &cm, true).await;
        let after_commit = deps
            .store
            .get_session_context(&deps.session_pk)
            .await
            .unwrap()
            .unwrap();
        let saved_after_commit = after_commit["models"]["test/model"].clone();
        assert_eq!(saved_after_commit["input"], 999);
        assert_eq!(saved_after_commit["output"], 7);
        assert_eq!(saved_after_commit["cache_read"], 3);

        // `cm.last_*` still report the SAME committed response (nothing
        // reset them) — exactly the stale-accessor condition at the
        // overflow/compact/fallback sites. The display-only re-emit must
        // NOT add them again.
        emit_context_display(&deps, &cm, true).await;
        let after_display = deps
            .store
            .get_session_context(&deps.session_pk)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            after_display["models"]["test/model"], saved_after_commit,
            "display-only re-emit must not change the persisted tally"
        );
    }

    #[tokio::test]
    async fn pre_turn_compaction_triggers_and_continues_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        // ScriptedLlm turn 0 answers the summarize call; turn 1 is the real turn.
        let summarize = vec![text_delta("compacted summary"), message_stop()];
        let main = vec![
            text_delta("done"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![summarize, main]));
        let mut deps = deps_at(dir.path(), llm).await;
        deps.meta = tiny_meta();
        // Preload enough history through the SAME store to exceed the tiny
        // 400-token window (each turn estimates to ~115 tokens).
        {
            let mut ledger = Ledger::load(deps.store.clone(), "s1").await.unwrap();
            for i in 0..4 {
                ledger
                    .append_user(
                        json!([{"type":"text","text": format!("u{i} {}", "x".repeat(400))}]),
                    )
                    .await
                    .unwrap();
                ledger
                    .append_assistant(json!([{"type":"text","text": format!("a{i}")}]))
                    .await
                    .unwrap();
            }
        }
        let mut rx = deps.events.subscribe();
        run_turn(
            &deps,
            TurnPrompt::text("next", "next"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        // Compaction happened: checkpoint row + event + turn still completed.
        assert!(deps
            .store
            .latest_context_checkpoint("s1")
            .await
            .unwrap()
            .is_some());
        let mut compacted = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, CoreEvent::ContextCompacted { .. }) {
                compacted = true;
            }
        }
        assert!(compacted);
        let msgs = deps.store.list_messages("s1").await.unwrap();
        assert!(msgs
            .iter()
            .any(|m| m.block_type == "text" && m.payload["text"] == "done"));
    }

    #[tokio::test]
    async fn overflow_error_marks_context_full_and_surfaces_error() {
        let dir = tempfile::tempdir().unwrap();
        let turn = vec![error_event(
            "prompt is too long: 500000 tokens > 400000 maximum",
        )];
        let llm = Arc::new(ScriptedLlm::new(vec![turn]));
        let deps = deps_at(dir.path(), llm).await;
        let err = run_turn(&deps, TurnPrompt::text("x", "x"), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("context"));
        let ctx = deps.store.get_session_context("s1").await.unwrap().unwrap();
        assert_eq!(ctx["percent_left"], 0);
    }

    #[tokio::test]
    async fn overflow_then_next_turn_compacts_deterministically() {
        let dir = tempfile::tempdir().unwrap();
        // Turn 1 overflows: mark_full pins the in-memory indicator to 0%
        // and persists the full-window total to session_context.
        let overflow = vec![error_event(
            "prompt is too long: 500000 tokens > 400000 maximum",
        )];
        // Turn 2: the pre-turn compaction check fires BEFORE the real model
        // call, so the scripted order is summarize-then-main.
        let summarize = vec![text_delta("compacted summary"), message_stop()];
        let main = vec![
            text_delta("done"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![overflow, summarize, main]));
        // deps_at defaults to FALLBACK meta (128k window) — deliberately NOT
        // a tiny meta, so the turn-2 reload estimate (just the one tiny "x"
        // user turn left over from turn 1) sits at well under 1% of the
        // window and would NOT, by itself, cross the 90% auto-compact
        // threshold. Only the seeded full-window total proves the fix.
        let deps = deps_at(dir.path(), llm).await;

        let err = run_turn(&deps, TurnPrompt::text("x", "x"), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("context"));
        assert!(
            deps.store
                .latest_context_checkpoint("s1")
                .await
                .unwrap()
                .is_none(),
            "turn 1 errored before any compaction ran"
        );

        // Turn 2: the ContextManager rebuilt from the reloaded (tiny) ledger
        // must be seeded with turn 1's persisted full-window total so the
        // pre-turn check deterministically compacts instead of looping on
        // the undercounted reload estimate.
        run_turn(
            &deps,
            TurnPrompt::text("next", "next"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        assert!(
            deps.store
                .latest_context_checkpoint("s1")
                .await
                .unwrap()
                .is_some(),
            "pre-turn compaction must fire off the seeded overflow state"
        );
        let msgs = deps.store.list_messages("s1").await.unwrap();
        assert!(msgs
            .iter()
            .any(|m| m.block_type == "text" && m.payload["text"] == "done"));
    }

    #[tokio::test]
    async fn manual_compact_command_compacts_without_a_model_turn() {
        let dir = tempfile::tempdir().unwrap();
        let summarize = vec![text_delta("manual summary"), message_stop()];
        let llm = Arc::new(ScriptedLlm::new(vec![summarize]));
        let deps = deps_at(dir.path(), llm).await;
        {
            let mut ledger = Ledger::load(deps.store.clone(), "s1").await.unwrap();
            ledger
                .append_user(json!([{"type":"text","text":"old question"}]))
                .await
                .unwrap();
            ledger
                .append_assistant(json!([{"type":"text","text":"old answer"}]))
                .await
                .unwrap();
        }
        run_turn(
            &deps,
            TurnPrompt::text("/compact", "/compact"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let ck = deps.store.latest_context_checkpoint("s1").await.unwrap();
        assert!(ck.is_some(), "manual /compact wrote a checkpoint");
        // A notice row records it in the transcript.
        let msgs = deps.store.list_messages("s1").await.unwrap();
        assert!(msgs.iter().any(|m| m.block_type == "notice"));
    }

    #[tokio::test]
    async fn cache_control_and_max_tokens_follow_model_meta() {
        use testutil::RecordingLlm;
        let dir = tempfile::tempdir().unwrap();
        let turn = vec![text_delta("ok"), message_delta("end_turn"), message_stop()];
        let llm = Arc::new(RecordingLlm::new(vec![turn]));
        let mut deps = deps_at(dir.path(), llm.clone()).await;
        deps.meta = crate::llm_router::model_meta::ModelMeta {
            context_window: 200_000,
            max_output_tokens: 64_000,
            supports_prompt_cache: true,
            supports_reasoning: true,
            cost_input: 0.0,
            cost_output: 0.0,
            cost_cache_read: 0.0,
            cost_cache_write: 0.0,
        };
        deps.effort = Some("high".into());
        run_turn(&deps, TurnPrompt::text("x", "x"), CancellationToken::new())
            .await
            .unwrap();
        let bodies = llm.bodies.lock().unwrap();
        let body = &bodies[0];
        assert_eq!(body["max_tokens"], 64_000);
        // System is a cache-controlled block array.
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        // Moving breakpoint on the final message's last block.
        let msgs = body["messages"].as_array().unwrap();
        let last_blocks = msgs.last().unwrap()["content"].as_array().unwrap();
        assert_eq!(
            last_blocks.last().unwrap()["cache_control"]["type"],
            "ephemeral"
        );
        // Effort high → extended thinking budget.
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 16_384);
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
            TurnPrompt::text("please write out.txt", "please write out.txt"),
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
    async fn mid_turn_steer_is_injected_into_the_next_tool_result_batch() {
        use super::super::steer::{STEER_MARKER_CLOSE, STEER_MARKER_OPEN};
        use testutil::RecordingLlm;
        let dir = tempfile::tempdir().unwrap();
        // Turn 1: one tool call (bash), no tool-less text.
        let turn1 = vec![
            tool_use_start(1, "call-1", "bash"),
            input_json_delta(1, "{\"command\":\"echo hi\"}"),
            message_delta("tool_use"),
            message_stop(),
        ];
        // Turn 2: acknowledges and ends.
        let turn2 = vec![text_delta("ok"), message_delta("end_turn"), message_stop()];
        let llm = Arc::new(RecordingLlm::new(vec![turn1, turn2]));
        let deps = deps_at(dir.path(), llm.clone()).await;

        // A `steer()` call landing while the tool call above is executing
        // pushes onto the SAME buffer `drive()` drains — exactly what
        // `NativeSession::steer` does from a concurrent `steer` RPC. Pushed
        // here (before the turn starts) is equivalent: `take_block()` picks
        // up whatever is queued the instant `drive()` reaches the drain
        // point, regardless of exactly when the push landed.
        deps.steer.push("stop and check the tests first".into());

        run_turn(
            &deps,
            TurnPrompt::text("run the tests", "run the tests"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let bodies = llm.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "the tool round, then the follow-up call");
        // The follow-up request's LAST message is the drained steer block —
        // appended right after the tool-result user turn, so the model sees
        // it on this, the NEXT, iteration.
        let messages = bodies[1]["messages"].as_array().unwrap();
        let last = messages.last().expect("at least one message");
        assert_eq!(last["role"], "user");
        let rendered = serde_json::to_string(last).unwrap();
        assert!(rendered.contains(STEER_MARKER_OPEN));
        assert!(rendered.contains(STEER_MARKER_CLOSE));
        assert!(rendered.contains("stop and check the tests first"));

        // Drained: a later turn would not see it again.
        assert!(deps.steer.take_block().is_none());
    }

    #[tokio::test]
    async fn steer_on_a_tool_less_turn_forces_a_delivery_round() {
        use super::super::steer::{STEER_MARKER_CLOSE, STEER_MARKER_OPEN};
        use testutil::RecordingLlm;
        let dir = tempfile::tempdir().unwrap();
        // Turn 1: plain-text answer, no tool call — the model would end here.
        let turn1 = vec![
            text_delta("done"),
            message_delta("end_turn"),
            message_stop(),
        ];
        // Turn 2: the steer forced one more round; the model acknowledges + ends.
        let turn2 = vec![
            text_delta("ok, stopping"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = Arc::new(RecordingLlm::new(vec![turn1, turn2]));
        let deps = deps_at(dir.path(), llm.clone()).await;

        deps.steer.push("actually, stop".into());

        run_turn(
            &deps,
            TurnPrompt::text("go", "go"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let bodies = llm.bodies.lock().unwrap();
        assert_eq!(
            bodies.len(),
            2,
            "the tool-less turn, then the forced steer-delivery round"
        );
        // The second request carries the drained steer block as its last
        // message — the model gets to answer the steer, not drop it.
        let messages = bodies[1]["messages"].as_array().unwrap();
        let last = messages.last().expect("at least one message");
        assert_eq!(last["role"], "user");
        let rendered = serde_json::to_string(last).unwrap();
        assert!(rendered.contains(STEER_MARKER_OPEN));
        assert!(rendered.contains(STEER_MARKER_CLOSE));
        assert!(rendered.contains("actually, stop"));
        // Drained exactly once — a later turn will not see it again.
        assert!(deps.steer.take_block().is_none());
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
        let err = run_turn(&deps, TurnPrompt::text("x", "x"), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn task_tool_spawns_subagent_and_returns_its_report() {
        let dir = tempfile::tempdir().unwrap();
        // Parent turn: call the `task` tool delegating to `explore`.
        let parent = vec![
            tool_use_start(0, "call-1", "task"),
            input_json_delta(
                0,
                "{\"subagent_type\":\"explore\",\"prompt\":\"find the readme\"}",
            ),
            message_delta("tool_use"),
            message_stop(),
        ];
        // After the tool_result comes back, the parent ends the turn.
        let parent_end = vec![
            text_delta("all set"),
            message_delta("end_turn"),
            message_stop(),
        ];
        // The sub-agent (explore) runs one turn and reports.
        let sub = vec![
            text_delta("The readme is README.md"),
            message_delta("end_turn"),
            message_stop(),
        ];
        // ScriptedLlm serves turns in order across BOTH parent and sub-agent
        // stream() calls: parent turn 1, then the sub-agent's turn, then the
        // parent's continuation.
        let llm = Arc::new(ScriptedLlm::new(vec![parent, sub, parent_end]));
        let deps = deps_at(dir.path(), llm).await;

        run_turn(
            &deps,
            TurnPrompt::text("where is the readme?", "where is the readme?"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let msgs = deps.store.list_messages("s1").await.unwrap();
        // The task tool_call row carries the sub-agent's report as output.
        let task_row = msgs
            .iter()
            .find(|m| m.block_type == "tool_call" && m.payload["name"] == "task")
            .expect("a task tool_call row");
        assert_eq!(task_row.status.as_deref(), Some("completed"));
        assert!(task_row.payload["output"]
            .as_str()
            .unwrap()
            .contains("README.md"));
        // The sub-agent's internal text is NOT persisted as a parent row.
        assert!(!msgs
            .iter()
            .any(|m| m.block_type == "text" && m.payload["text"] == "The readme is README.md"));
        // The parent's own closing text is present.
        assert!(msgs
            .iter()
            .any(|m| m.block_type == "text" && m.payload["text"] == "all set"));
    }

    #[test]
    fn merge_display_duration_folds_duration_into_existing_extras() {
        let merged = merge_display_duration(Some(json!({ "summary": "todos: 1/2 done" })), 1234);
        assert_eq!(
            merged,
            json!({ "summary": "todos: 1/2 done", "duration_ms": 1234 })
        );
    }

    #[test]
    fn merge_display_duration_handles_missing_or_non_object_extras() {
        assert_eq!(merge_display_duration(None, 7), json!({ "duration_ms": 7 }));
        // A non-object display value would corrupt the json_patch — drop it.
        assert_eq!(
            merge_display_duration(Some(json!("junk")), 7),
            json!({ "duration_ms": 7 })
        );
    }

    #[tokio::test]
    async fn tool_call_payload_carries_duration_and_display_extras() {
        let dir = tempfile::tempdir().unwrap();
        // todowrite exercises timing + summary extras WITHOUT spawning any
        // process (bash-based turns fail on sh-less Windows dev boxes).
        let turn1 = vec![
            tool_use_start(0, "call-1", "todowrite"),
            input_json_delta(
                0,
                "{\"todos\":[{\"content\":\"first\",\"status\":\"completed\"},{\"content\":\"second\",\"status\":\"pending\"}]}",
            ),
            message_delta("tool_use"),
            message_stop(),
        ];
        let turn2 = vec![text_delta("ok"), message_delta("end_turn"), message_stop()];
        let llm = Arc::new(ScriptedLlm::new(vec![turn1, turn2]));
        let deps = deps_at(dir.path(), llm).await;

        run_turn(
            &deps,
            TurnPrompt::text("plan it", "plan it"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let msgs = deps.store.list_messages("s1").await.unwrap();
        let row = msgs
            .iter()
            .find(|m| m.block_type == "tool_call")
            .expect("a tool_call row");
        assert_eq!(row.payload["name"], "todowrite");
        // The tool's own display extras still land in the payload...
        assert_eq!(row.payload["summary"], "todos: 1/2 done");
        // ...and the runner's timing is merged in beside them.
        assert!(
            row.payload["duration_ms"].is_u64(),
            "payload missing duration_ms: {}",
            row.payload
        );
    }

    #[test]
    fn cap_report_truncates_head_and_tail_with_marker() {
        let long = "x".repeat(40_000);
        let capped = cap_report(&long);
        assert!(capped.chars().count() < MAX_SUBTASK_REPORT_CHARS + 100);
        assert!(capped.contains("chars elided"));
        // Small reports pass through unchanged.
        assert_eq!(cap_report("short"), "short");
    }

    #[test]
    fn effective_child_filter_intersects_and_blocks() {
        use super::super::agents::ToolFilter;
        let names: Vec<String> = ["read", "bash", "task", "memory", "grep"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parent = ToolFilter::Only(vec!["read".into(), "task".into(), "bash".into()]);
        let eff = effective_child_filter(&parent, &ToolFilter::All, &names, SUBAGENT_BLOCKLIST);
        assert!(eff.allows("read") && eff.allows("bash"));
        assert!(!eff.allows("task"), "blocklist wins over parent allow");
        assert!(!eff.allows("memory"));
        assert!(!eff.allows("grep"), "parent filter constrains the child");
        // All ∩ All − blocklist keeps everything else.
        let eff = effective_child_filter(&ToolFilter::All, &ToolFilter::All, &names, &["memory"]);
        assert!(eff.allows("task") && eff.allows("read"));
        assert!(!eff.allows("memory"));
    }

    #[test]
    fn subagent_blocklist_blocks_todo_tools() {
        use super::super::agents::ToolFilter;
        let names: Vec<String> = ["read", "bash", "todowrite", "todoread"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let eff = effective_child_filter(
            &ToolFilter::All,
            &ToolFilter::All,
            &names,
            SUBAGENT_BLOCKLIST,
        );
        assert!(eff.allows("read") && eff.allows("bash"));
        assert!(
            !eff.allows("todowrite"),
            "a sub-agent todowrite would clobber the parent session's plan"
        );
        assert!(!eff.allows("todoread"));
    }

    #[tokio::test]
    async fn run_many_serial_is_ordered_and_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let child_a = vec![
            text_delta("report A"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let child_b = vec![
            text_delta("report B"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![child_a, child_b]));
        let deps = deps_at(dir.path(), llm).await;
        // Serialize children so the scripted turns map deterministically.
        deps.store
            .set_setting("max_concurrent_runs", "1")
            .await
            .unwrap();
        let spawner = RunnerSpawner {
            deps: deps.clone(),
            cancel: CancellationToken::new(),
            depth: 0,
        };
        let results = spawner
            .run_many(vec![
                SubtaskSpec {
                    agent_type: "explore".into(),
                    prompt: "first".into(),
                },
                SubtaskSpec {
                    agent_type: "explore".into(),
                    prompt: "second".into(),
                },
            ])
            .await;
        assert_eq!(results.len(), 2);
        assert_eq!((results[0].index, results[1].index), (0, 1));
        assert!(results.iter().all(|r| r.status == SubtaskStatus::Completed));
        assert_eq!(results[0].report, "report A");
        assert_eq!(results[1].report, "report B");
    }

    #[tokio::test]
    async fn run_many_concurrent_batch_completes_all() {
        use crate::llm_router::client::AnthropicEvent;
        let dir = tempfile::tempdir().unwrap();
        let turns: Vec<Vec<AnthropicEvent>> = (0..3)
            .map(|_| {
                vec![
                    text_delta("done"),
                    message_delta("end_turn"),
                    message_stop(),
                ]
            })
            .collect();
        let llm = Arc::new(ScriptedLlm::new(turns));
        let deps = deps_at(dir.path(), llm).await;
        let spawner = RunnerSpawner {
            deps,
            cancel: CancellationToken::new(),
            depth: 0,
        };
        let specs = (0..3)
            .map(|i| SubtaskSpec {
                agent_type: "explore".into(),
                prompt: format!("job {i}"),
            })
            .collect();
        let results = spawner.run_many(specs).await;
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.status == SubtaskStatus::Completed));
        assert_eq!(
            results.iter().map(|r| r.index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[tokio::test]
    async fn run_many_isolates_individual_failures() {
        let dir = tempfile::tempdir().unwrap();
        let child = vec![
            text_delta("fine"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![child]));
        let deps = deps_at(dir.path(), llm).await;
        let spawner = RunnerSpawner {
            deps,
            cancel: CancellationToken::new(),
            depth: 0,
        };
        let results = spawner
            .run_many(vec![
                SubtaskSpec {
                    agent_type: "no-such-agent".into(),
                    prompt: "x".into(),
                },
                SubtaskSpec {
                    agent_type: "explore".into(),
                    prompt: "y".into(),
                },
            ])
            .await;
        assert_eq!(results[0].status, SubtaskStatus::Error);
        assert!(results[0].report.contains("unknown sub-agent"));
        assert!(results[0].report.contains("explore"), "lists available");
        assert_eq!(results[1].status, SubtaskStatus::Completed);
        assert_eq!(results[1].report, "fine");
    }

    #[tokio::test]
    async fn run_many_precancelled_yields_interrupted_entries() {
        let dir = tempfile::tempdir().unwrap();
        // No scripted turns: a model call would error the test.
        let llm = Arc::new(ScriptedLlm::new(vec![]));
        let deps = deps_at(dir.path(), llm).await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let spawner = RunnerSpawner {
            deps,
            cancel,
            depth: 0,
        };
        let results = spawner
            .run_many(vec![
                SubtaskSpec {
                    agent_type: "explore".into(),
                    prompt: "a".into(),
                },
                SubtaskSpec {
                    agent_type: "explore".into(),
                    prompt: "b".into(),
                },
            ])
            .await;
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|r| r.status == SubtaskStatus::Interrupted));
    }

    #[tokio::test]
    async fn orchestrator_child_delegates_at_default_depth() {
        use testutil::RecordingLlm;
        let dir = tempfile::tempdir().unwrap();
        // parent → task(orchestrator) → orchestrator → task(explore) →
        // explore reports → orchestrator synthesizes → parent closes.
        let parent = vec![
            tool_use_start(0, "c1", "task"),
            input_json_delta(
                0,
                "{\"subagent_type\":\"orchestrator\",\"prompt\":\"coordinate this\"}",
            ),
            message_delta("tool_use"),
            message_stop(),
        ];
        let orch_1 = vec![
            tool_use_start(0, "c2", "task"),
            input_json_delta(
                0,
                "{\"subagent_type\":\"explore\",\"prompt\":\"look around\"}",
            ),
            message_delta("tool_use"),
            message_stop(),
        ];
        let explore = vec![
            text_delta("explored ok"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let orch_2 = vec![
            text_delta("coordinated: explored ok"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let parent_end = vec![
            text_delta("done"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = Arc::new(RecordingLlm::new(vec![
            parent, orch_1, explore, orch_2, parent_end,
        ]));
        // max_spawn_depth unset: the default (2) must let the builtin
        // orchestrator delegate out of the box.
        let deps = deps_at(dir.path(), llm.clone()).await;

        run_turn(
            &deps,
            TurnPrompt::text("go wide", "go wide"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        {
            let bodies = llm.bodies.lock().unwrap();
            assert_eq!(bodies.len(), 5, "orchestrator's delegation really ran");
            // The orchestrator child carries the capability block + task tool.
            let orch_sys = bodies[1]["system"].as_str().unwrap();
            assert!(orch_sys.contains("spawn depth 1 of 2"), "{orch_sys}");
            let orch_tools: Vec<&str> = bodies[1]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|t| t["name"].as_str())
                .collect();
            assert!(orch_tools.contains(&"task"));
            assert!(!orch_tools.contains(&"memory"), "memory stays blocked");
            // Guard dropped before the awaits below (clippy: await_holding_lock).
        }
        // The grandchild explore ran and fed the orchestrator's synthesis.
        let task_row_out = deps
            .store
            .list_messages("s1")
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.block_type == "tool_call" && m.payload["name"] == "task")
            .expect("task row")
            .payload["output"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(task_row_out.contains("coordinated: explored ok"));
    }

    #[tokio::test]
    async fn orchestrator_child_cannot_delegate_when_depth_is_flat() {
        use testutil::RecordingLlm;
        let dir = tempfile::tempdir().unwrap();
        let parent = vec![
            tool_use_start(0, "c1", "task"),
            input_json_delta(
                0,
                "{\"subagent_type\":\"orchestrator\",\"prompt\":\"coordinate\"}",
            ),
            message_delta("tool_use"),
            message_stop(),
        ];
        // The orchestrator tries to delegate anyway; the tool must refuse.
        let orch_try = vec![
            tool_use_start(0, "c2", "task"),
            input_json_delta(0, "{\"subagent_type\":\"explore\",\"prompt\":\"x\"}"),
            message_delta("tool_use"),
            message_stop(),
        ];
        let orch_give_up = vec![
            text_delta("did it alone"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let parent_end = vec![text_delta("ok"), message_delta("end_turn"), message_stop()];
        let llm = Arc::new(RecordingLlm::new(vec![
            parent,
            orch_try,
            orch_give_up,
            parent_end,
        ]));
        let deps = deps_at(dir.path(), llm.clone()).await;
        // Explicit flat delegation: the orchestrator child may not re-spawn.
        deps.store
            .set_setting("max_spawn_depth", "1")
            .await
            .unwrap();

        run_turn(
            &deps,
            TurnPrompt::text("go", "go"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let bodies = llm.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 4, "no grandchild stream was opened");
        // `task` was filtered out of the child's toolset entirely, so the
        // allow-list gate refuses the attempt.
        let orch_tools: Vec<&str> = bodies[1]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(!orch_tools.contains(&"task"));
        let msgs = serde_json::to_string(&bodies[2]["messages"]).unwrap();
        assert!(msgs.contains("not permitted"), "{msgs}");
        // And its system prompt has no capability block.
        assert!(!bodies[1]["system"]
            .as_str()
            .unwrap()
            .contains("spawn depth"));
    }

    #[tokio::test]
    async fn memory_snapshot_reaches_primary_system_but_not_subagents() {
        use crate::harness::native::memory::{MemoryScope, MemoryStore};
        use testutil::RecordingLlm;
        let dir = tempfile::tempdir().unwrap();
        // Parent calls task -> child explore runs -> parent closes.
        let parent = vec![
            tool_use_start(0, "c1", "task"),
            input_json_delta(0, "{\"subagent_type\":\"explore\",\"prompt\":\"look\"}"),
            message_delta("tool_use"),
            message_stop(),
        ];
        let sub = vec![
            text_delta("found"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let parent_end = vec![
            text_delta("done"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = Arc::new(RecordingLlm::new(vec![parent, sub, parent_end]));
        let mut deps = deps_at(dir.path(), llm.clone()).await;
        let memdir = tempfile::tempdir().unwrap();
        let mem = MemoryStore::new(memdir.path().join("MEMORY.md"), None);
        mem.add(MemoryScope::Global, "remember: the repo uses bun")
            .unwrap();
        deps.memory = Some(Arc::new(mem));

        run_turn(
            &deps,
            TurnPrompt::text("go", "go"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let bodies = llm.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 3);
        let parent_sys = bodies[0]["system"].as_str().unwrap();
        assert!(parent_sys.contains("remember: the repo uses bun"));
        assert!(parent_sys.contains("# Persistent memory (global)"));
        // No child request may carry the memory text (sub-agents run
        // memoryless).
        let child_sys = bodies[1]["system"].as_str().unwrap();
        assert!(!child_sys.contains("remember: the repo uses bun"));
        // The parent continuation keeps it.
        assert!(bodies[2]["system"]
            .as_str()
            .unwrap()
            .contains("remember: the repo uses bun"));
    }

    #[tokio::test]
    async fn generates_a_title_for_a_fresh_session() {
        use crate::domain::{Project, Session, SessionKind, SessionStatus};
        let dir = tempfile::tempdir().unwrap();
        // Turn 0: the actual reply. Turn 1: the title generation.
        let main = vec![
            text_delta("done"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let title = vec![
            text_delta("Fix the "),
            text_delta("login bug"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![main, title]));
        let deps = deps_at(dir.path(), llm).await;
        // A session row must exist (and be untitled) for title generation.
        deps.store
            .insert_project(Project {
                project_id: "p".into(),
                name: "p".into(),
                workdir: dir.path().to_string_lossy().into(),
                source: None,
                harness: "native".into(),
                model: None,
                effort: None,
                perm_mode: PermMode::Default,
                created_at: Some(0),
                is_git: false,
            })
            .await
            .unwrap();
        deps.store
            .insert_session(Session {
                session_pk: "s1".into(),
                project_id: Some("p".into()),
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: None,
                status: SessionStatus::Running,
                perm_mode: PermMode::Default,
                started_by: None,
                created_at: Some(0),
                last_active: Some(0),
                resume_attempts: 0,
                branch_owned: true,
                kind: SessionKind::Project,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();

        run_turn(
            &deps,
            TurnPrompt::text("fix login", "fix login"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let session = deps.store.get_session("s1").await.unwrap().unwrap();
        assert_eq!(session.title.as_deref(), Some("Fix the login bug"));
    }

    #[tokio::test]
    async fn slash_command_expands_and_switches_agent() {
        let dir = tempfile::tempdir().unwrap();
        // /review pins the plan agent (read-only). The model just ends the turn.
        let turn = vec![
            text_delta("reviewed"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![turn]));
        let deps = deps_at(dir.path(), llm).await;

        run_turn(
            &deps,
            TurnPrompt::text("/review", "/review"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        // The display row keeps the raw "/review"; the ledger's user turn holds
        // the expanded template.
        let msgs = deps.store.list_messages("s1").await.unwrap();
        assert_eq!(msgs[0].payload["text"], "/review");
        let turns = deps.store.list_provider_turns("s1").await.unwrap();
        assert!(turns[0].payload[0]["text"]
            .as_str()
            .unwrap()
            .contains("Review the current working changes"));
    }

    #[tokio::test]
    async fn slash_command_expansion_preserves_agent_prompt_context() {
        let dir = tempfile::tempdir().unwrap();
        let turn = vec![
            text_delta("reviewed"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![turn]));
        let deps = deps_at(dir.path(), llm).await;

        run_turn(
            &deps,
            TurnPrompt::text(
                "/review auth\n\n[Chat context]\n- Branch: feature/auth\n\n[User attached 1 file - saved to disk:]",
                "/review auth",
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let turns = deps.store.list_provider_turns("s1").await.unwrap();
        let user_text = turns[0].payload[0]["text"].as_str().unwrap();
        assert!(user_text.contains("Review the current working changes"));
        assert!(user_text.contains("auth"));
        assert!(user_text.contains("[Chat context]"));
        assert!(user_text.contains("feature/auth"));
        assert!(user_text.contains("[User attached 1 file"));
    }

    #[test]
    fn user_row_payload_omits_attachments_when_empty_and_includes_them_when_present() {
        let plain = TurnPrompt::text("hi", "hi");
        assert_eq!(user_row_payload(&plain), json!({ "text": "hi" }));

        let with = TurnPrompt {
            attachments: vec![
                json!({ "name": "a.png", "path": "/x/a.png", "contentType": "image/png", "size": 4 }),
            ],
            ..TurnPrompt::text("hi", "hi")
        };
        let payload = user_row_payload(&with);
        assert_eq!(payload["text"], "hi");
        assert_eq!(payload["attachments"][0]["name"], "a.png");
    }

    #[test]
    fn user_content_blocks_prepends_image_blocks_before_the_text_block() {
        let img = json!({ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AA==" } });
        let content = user_content_blocks(std::slice::from_ref(&img), "look at this");
        let arr = content.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0], img);
        assert_eq!(arr[1], json!({ "type": "text", "text": "look at this" }));
    }

    #[tokio::test]
    async fn precancelled_turn_returns_without_calling_model() {
        let dir = tempfile::tempdir().unwrap();
        // No scripted turns: if the loop called the model it would error.
        let llm = Arc::new(ScriptedLlm::new(vec![]));
        let deps = deps_at(dir.path(), llm).await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        run_turn(&deps, TurnPrompt::text("x", "x"), cancel)
            .await
            .unwrap();
        // The user row was still persisted before the cancel check.
        let msgs = deps.store.list_messages("s1").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
    }

    #[tokio::test]
    async fn request_body_repairs_a_dangling_tool_use_from_a_prior_interrupted_turn() {
        let dir = tempfile::tempdir().unwrap();
        let turn = vec![text_delta("ok"), message_delta("end_turn"), message_stop()];
        let llm = Arc::new(RecordingLlm::new(vec![turn]));
        let deps = deps_at(dir.path(), llm.clone()).await;
        // Simulate a prior turn interrupted mid-tools: the assistant tool_use
        // turn was persisted but its tool_result user turn never was.
        {
            let mut ledger = Ledger::load(deps.store.clone(), "s1").await.unwrap();
            ledger
                .append_user(json!([{"type": "text", "text": "earlier"}]))
                .await
                .unwrap();
            ledger
                .append_assistant(json!([
                    {"type": "tool_use", "id": "tu-dangling", "name": "bash", "input": {}}
                ]))
                .await
                .unwrap();
        }

        run_turn(
            &deps,
            TurnPrompt::text("next", "next"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let bodies = llm.bodies.lock().unwrap();
        let messages = bodies[0]["messages"].as_array().unwrap();
        // user(earlier), assistant(tool_use), user(tool_result + "next") —
        // the repair is folded into the immediately-following user message.
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "tu-dangling");
        assert_eq!(messages[2]["content"][0]["is_error"], true);
        assert_eq!(messages[2]["content"][1]["type"], "text");
        assert_eq!(messages[2]["content"][1]["text"], "next");
    }

    #[tokio::test]
    async fn cancel_during_parked_approval_still_appends_a_paired_tool_result() {
        let dir = tempfile::tempdir().unwrap();
        let turn = vec![
            tool_use_start(0, "call-park", "bash"),
            input_json_delta(0, "{\"command\":\"echo hi\"}"),
            message_delta("tool_use"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![turn]));
        let deps = deps_at(dir.path(), llm).await;
        // Default mode: bash prompts, and nobody will ever answer.
        deps.set_perm_mode(PermMode::Default);
        let mut rx = deps.events.subscribe();
        let cancel = CancellationToken::new();
        let run = {
            let deps = deps.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                run_turn(&deps, TurnPrompt::text("run it", "run it"), cancel).await
            })
        };
        // Wait for the approval prompt, then stop the turn instead of answering.
        loop {
            if let CoreEvent::ApprovalRequested { request_id, .. } = rx.recv().await.unwrap() {
                assert_eq!(request_id, "call-park");
                break;
            }
        }
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), run)
            .await
            .expect("turn must settle after cancel (approval gate must observe the turn token)")
            .unwrap()
            .unwrap();

        // The ledger is PAIRED: user, assistant(tool_use), user(tool_result).
        let turns = deps.store.list_provider_turns("s1").await.unwrap();
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[1].role, "assistant");
        assert_eq!(turns[2].role, "user");
        assert_eq!(turns[2].payload[0]["type"], "tool_result");
        assert_eq!(turns[2].payload[0]["tool_use_id"], "call-park");
        assert_eq!(turns[2].payload[0]["is_error"], true);
        assert!(turns[2].payload[0]["content"]
            .as_str()
            .unwrap()
            .contains("Interrupted"));
    }

    #[tokio::test]
    async fn budget_exhaustion_emits_a_summary_not_a_bare_notice() {
        // A tiny budget of 2: two scripted turns ALWAYS return a tool_use (so
        // neither hits the `tool_calls.is_empty()` end_turn return), which
        // drives `try_consume()` to genuine exhaustion on the loop's third
        // attempt — this also closes the B1 gap of never having exercised
        // that path end-to-end. A THIRD scripted, tool-less turn is the
        // post-exhaustion summary call.
        use testutil::RecordingLlm;
        let dir = tempfile::tempdir().unwrap();
        let tool_turn = |call_id: &str| {
            vec![
                tool_use_start(0, call_id, "bash"),
                input_json_delta(0, "{\"command\":\"echo hi\"}"),
                message_delta("tool_use"),
                message_stop(),
            ]
        };
        let summary_turn = vec![
            text_delta("Summary: explored the repo and made no changes."),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = Arc::new(RecordingLlm::new(vec![
            tool_turn("call-1"),
            tool_turn("call-2"),
            summary_turn,
        ]));
        let deps = deps_at(dir.path(), llm.clone()).await;
        // drive() runs here at DisplayMode::Full (top-level), where budget
        // exhaustion would now trigger auto-continue; disable it so the run
        // still reaches the summary tail this test asserts on.
        deps.store
            .set_setting("agent.auto_continue_budget", "0")
            .await
            .unwrap();
        let agent = deps.agent.clone();
        let mut cm =
            ContextManager::ephemeral(&deps.session_pk, ContextConfig::with_meta(deps.meta));
        cm.append_user(json!([{ "type": "text", "text": "keep going forever" }]))
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        let budget = IterationBudget::new(2);

        let text = drive(
            &deps,
            &agent,
            &mut cm,
            &cancel,
            None,
            DisplayMode::Full,
            &budget,
        )
        .await
        .unwrap();

        // The bare "Turn limit reached" sentinel is gone; drive() returns the
        // model's actual summary text instead.
        assert_eq!(text, "Summary: explored the repo and made no changes.");
        assert!(!text.contains("Turn limit reached"));

        // Exactly 3 requests went out: 2 tool-calling turns + 1 summary call.
        let bodies = llm.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 3, "2 budgeted turns + 1 summary call");
        // The summary call must be tool-less (no tools offered).
        let summary_body = &bodies[2];
        let tools_empty = summary_body
            .get("tools")
            .map(|t| t.as_array().is_none_or(|a| a.is_empty()))
            .unwrap_or(true);
        assert!(
            tools_empty,
            "summary call must not offer tools: {summary_body}"
        );
        // ... and it carries the budget-exhausted nudge as its final user turn.
        let messages = summary_body["messages"].as_array().unwrap();
        let last_text = messages.last().unwrap()["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(last_text.contains("maximum number of tool-calling iterations"));
    }

    /// With max_provider_turns=1 and auto_continue_budget=1: turn 1 is a tool
    /// call (exhausts the 1-turn budget window), the loop auto-continues once
    /// with a notice + synthetic "continue" user turn, and turn 2 ends normally.
    #[tokio::test]
    async fn turn_limit_auto_continues_with_budget() {
        let dir = tempfile::tempdir().unwrap();
        let turn1 = vec![
            tool_use_start(0, "t1", "ls"),
            input_json_delta(0, r#"{"path":"."}"#),
            message_delta("tool_use"),
            message_stop(),
        ];
        let turn2 = vec![
            text_delta("done"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![turn1, turn2]));
        let deps = deps_at(dir.path(), llm).await;
        seed_pinned_project(&deps.store, Some("anthropic/model-a")).await;
        add_anthropic_conn(&deps.store, &["model-a"]).await;
        deps.store
            .set_setting("agent.max_provider_turns", "1")
            .await
            .unwrap();
        deps.store
            .set_setting("agent.auto_continue_budget", "1")
            .await
            .unwrap();

        let mut rx = deps.events.subscribe();
        run_turn(
            &deps,
            TurnPrompt::text("list files", "list files"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let mut notices: Vec<String> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let CoreEvent::Message {
                block_type,
                payload,
                ..
            } = ev
            {
                if block_type == "notice" {
                    notices.push(payload["text"].as_str().unwrap_or_default().to_string());
                }
            }
        }
        assert!(
            notices
                .iter()
                .any(|n| n.contains("continuing automatically (1/1)")),
            "expected auto-continue notice, got: {notices:?}"
        );
        // The synthetic continue turn must NOT be a display row — no user
        // "continue" message row is persisted (only the ledger grows).
        assert!(
            !notices.iter().any(|n| n.contains("send a message")),
            "budget was not exhausted, final stop notice must not appear: {notices:?}"
        );
    }

    /// Budget 0 disables auto-continue: exhausting the window emits ONLY the
    /// final "send a message" notice (legacy behavior).
    #[tokio::test]
    async fn turn_limit_stops_when_budget_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let turn1 = vec![
            tool_use_start(0, "t1", "ls"),
            input_json_delta(0, r#"{"path":"."}"#),
            message_delta("tool_use"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![turn1]));
        let deps = deps_at(dir.path(), llm).await;
        seed_pinned_project(&deps.store, Some("anthropic/model-a")).await;
        add_anthropic_conn(&deps.store, &["model-a"]).await;
        deps.store
            .set_setting("agent.max_provider_turns", "1")
            .await
            .unwrap();
        deps.store
            .set_setting("agent.auto_continue_budget", "0")
            .await
            .unwrap();

        let mut rx = deps.events.subscribe();
        run_turn(
            &deps,
            TurnPrompt::text("list files", "list files"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let mut notices: Vec<String> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let CoreEvent::Message {
                block_type,
                payload,
                ..
            } = ev
            {
                if block_type == "notice" {
                    notices.push(payload["text"].as_str().unwrap_or_default().to_string());
                }
            }
        }
        assert!(notices
            .iter()
            .any(|n| n.contains("send a message to continue")));
        assert!(!notices
            .iter()
            .any(|n| n.contains("continuing automatically")));
    }
}
