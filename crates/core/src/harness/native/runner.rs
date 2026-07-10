//! The native turn drain: one `run_turn` runs a prompt to completion, calling
//! the model, executing tools, and persisting + streaming everything through
//! the same [`CoreEvent`] surface the ACP harness uses.

use super::agents::{Agent, AgentRegistry};
use super::commands::CommandRegistry;
use super::ledger::Ledger;
use super::llm::LlmStream;
use super::permission::{evaluate, PermDecision};
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

/// Upper bound on provider turns per drain, to bound runaway tool loops.
const MAX_PROVIDER_TURNS: usize = 50;
/// `max_tokens` requested per provider turn.
const MAX_TOKENS: i64 = 8192;
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
    /// Plugin-bundled skill directories folded in beside the worktree/global
    /// ones (see `crate::plugins::PluginHost::enabled_skill_dirs`).
    pub extra_skill_dirs: Vec<PathBuf>,
    pub model: Option<String>,
    /// Interior-mutable so a LIVE session can pick up a permission-mode change
    /// (from the composer / project settings) on the NEXT turn without being
    /// torn down — the control plane refreshes it in the continue path. The
    /// tool gate reads it fresh per call via [`RunnerDeps::current_perm_mode`].
    pub perm_mode: Arc<std::sync::Mutex<PermMode>>,
    pub project_policy: Option<String>,
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

    // 2. Load history and append the user turn to the ledger.
    let mut ledger = Ledger::load(deps.store.clone(), &deps.session_pk).await?;
    ledger
        .append_user(user_content_blocks(&prompt.blocks, &agent_text))
        .await?;

    // 3. Drive the loop with a spawner available for the `task` tool.
    let spawn: Arc<dyn SubagentSpawner> = Arc::new(RunnerSpawner {
        deps: deps.clone(),
        cancel: cancel.clone(),
        depth: 0,
    });
    drive(deps, &agent, &mut ledger, &cancel, Some(spawn), true).await?;

    // 4. Best-effort: give a fresh session a generated title.
    maybe_generate_title(deps, &prompt.display).await;
    Ok(())
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

/// If this session has no title yet, generate a terse one from the first
/// prompt via a short non-streaming model call. Best-effort: any failure is
/// swallowed so it never affects the turn's outcome.
async fn maybe_generate_title(deps: &RunnerDeps, first_prompt: &str) {
    match deps.store.get_session(&deps.session_pk).await {
        Ok(Some(session)) if session.title.is_none() => {}
        _ => return, // no session row, or already titled
    }
    let model = deps.model.clone().unwrap_or_default();
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
    let Ok(mut rx) = deps.llm.stream(body).await else {
        return;
    };
    let mut title = String::new();
    while let Some(item) = rx.recv().await {
        if let Ok(ev) = item {
            if let Some(MessageStreamEvent::TextDelta { text, .. }) =
                MessageStreamEvent::from_event(&ev)
            {
                title.push_str(&text);
            }
        }
    }
    let title: String = title.trim().trim_matches('"').chars().take(80).collect();
    if !title.is_empty() {
        let _ = deps.store.set_session_title(&deps.session_pk, &title).await;
    }
}

/// The agentic provider-turn loop. Shared by the top-level turn and sub-agents.
/// `emit_display` gates persistence of display rows (off for sub-agents so
/// their internal steps don't clutter the parent transcript). Returns the
/// final assistant text.
async fn drive(
    deps: &RunnerDeps,
    agent: &Agent,
    ledger: &mut Ledger,
    cancel: &CancellationToken,
    spawn: Option<Arc<dyn SubagentSpawner>>,
    emit_display: bool,
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

    for _ in 0..MAX_PROVIDER_TURNS {
        if cancel.is_cancelled() {
            return Ok(final_text);
        }
        // Compact the in-memory history if it has grown past the token budget.
        super::compaction::maybe_compact(
            &deps.llm,
            &model,
            ledger,
            super::compaction::MAX_CONTEXT_TOKENS,
            super::compaction::KEEP_RECENT_USER_TURNS,
        )
        .await;
        let body = json!({
            "model": model,
            "system": system,
            // Sanitized projection: dangling tool_use ids from an interrupted
            // prior turn get synthesized error tool_results, or Anthropic
            // 400s the whole request (and the session stays poisoned).
            "messages": ledger.messages_for_request(),
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
                return Ok(final_text);
            }
            let ev = match item {
                Ok(ev) => ev,
                Err(e) => {
                    flush_text(deps, &mut text_buf, emit_display).await;
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
                        flush_text(deps, &mut text_buf, emit_display).await;
                    }
                }
                MessageStreamEvent::ThinkingDelta { text, .. } => {
                    if emit_display {
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
                MessageStreamEvent::MessageDelta { stop_reason, .. } => {
                    turn.stop_reason = stop_reason;
                }
                MessageStreamEvent::Error(msg) => {
                    flush_text(deps, &mut text_buf, emit_display).await;
                    anyhow::bail!("{msg}");
                }
                MessageStreamEvent::MessageStop => break,
                MessageStreamEvent::MessageStart(_)
                | MessageStreamEvent::ContentBlockStop { .. } => {}
            }
        }
        flush_text(deps, &mut text_buf, emit_display).await;
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
        ledger.append_assistant(json!(content)).await?;

        if tool_calls.is_empty() {
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
            results.push(run_tool_call(deps, agent, t, emit_display, &spawn, cancel).await);
        }
        ledger.append_user(json!(results)).await?;

        if cancel.is_cancelled() {
            return Ok(final_text);
        }
    }
    Ok(final_text)
}

/// Tools delegated children may never use regardless of filters. `task` is
/// re-armed for delegator agents (the orchestrator role); `memory` never is —
/// sub-agents run memoryless, mirroring hermes-agent's `skip_memory`.
const SUBAGENT_BLOCKLIST: &[&str] = &["task", "memory"];
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
        // No display rows, no memory access; history is ephemeral.
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
        let mut ledger = Ledger::ephemeral(&self.deps.session_pk);
        if let Err(e) = ledger
            .append_user(json!([{ "type": "text", "text": spec.prompt }]))
            .await
        {
            return result(SubtaskStatus::Error, e.to_string());
        }
        match drive(
            &child_deps,
            &child,
            &mut ledger,
            &cancel,
            child_spawn,
            false,
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
    emit_display: bool,
    spawn: &Option<Arc<dyn SubagentSpawner>>,
    cancel: &CancellationToken,
) -> Value {
    let input = t.parsed_input();
    let Some(tool) = deps.tools.get(&t.name) else {
        let msg = format!("unknown tool `{}`", t.name);
        if emit_display {
            insert_tool_row(deps, t, &input, "unknown").await;
            finish_tool_row(deps, &t.id, &msg, true).await;
        }
        return tool_result(&t.id, &msg, true);
    };
    // Enforce the agent's tool allow-list.
    if !agent.tools.allows(&t.name) {
        let msg = format!(
            "tool `{}` is not permitted for the `{}` agent",
            t.name, agent.name
        );
        if emit_display {
            insert_tool_row(deps, t, &input, tool.kind()).await;
            finish_tool_row(deps, &t.id, &msg, true).await;
        }
        return tool_result(&t.id, &msg, true);
    }
    if emit_display {
        insert_tool_row(deps, t, &input, tool.kind()).await;
    }

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
        if emit_display {
            finish_tool_row(deps, &t.id, &msg, true).await;
        }
        return tool_result(&t.id, &msg, true);
    }

    // Permission gate. Read the mode fresh so a mid-session change applies.
    let perm_mode = deps.current_perm_mode();
    let spec = tool.permission(&input);
    let decision = evaluate(
        &spec,
        perm_mode,
        deps.project_policy.as_deref(),
        &deps.session_pk,
        &t.id,
        &deps.approvals,
        &deps.events,
        cancel,
    )
    .await;
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
        if emit_display {
            finish_tool_row(deps, &t.id, msg, true).await;
        }
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

    // Execute.
    let ctx = ToolCtx {
        session_pk: deps.session_pk.clone(),
        work_dir: deps.work_dir.clone(),
        extra_skill_dirs: deps.extra_skill_dirs.clone(),
        store: deps.store.clone(),
        cancel: cancel.clone(),
        caps: OutputCaps::default(),
        spawn: spawn.clone(),
        memory: deps.memory.clone(),
        snapshots: deps.snapshots.clone(),
    };
    match tool.execute(&ctx, input).await {
        Ok(out) => {
            if emit_display {
                finish_tool_row_with_display(
                    deps,
                    &t.id,
                    &out.for_model,
                    out.is_error,
                    out.display,
                )
                .await;
            }
            tool_result(&t.id, &out.for_model, out.is_error)
        }
        Err(e) => {
            let msg = format!("{}: {e}", t.name);
            if emit_display {
                finish_tool_row(deps, &t.id, &msg, true).await;
            }
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
        let agents = Arc::new(AgentRegistry::builtin());
        let agent = agents.default_agent();
        RunnerDeps {
            session_pk: "s1".into(),
            work_dir: dir.to_path_buf(),
            extra_skill_dirs: vec![],
            // bypassPermissions so the scripted bash tool runs without a prompt.
            model: Some("test/model".into()),
            perm_mode: Arc::new(std::sync::Mutex::new(PermMode::BypassPermissions)),
            project_policy: None,
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
        use crate::domain::{Project, Session, SessionStatus};
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
                project_id: "p".into(),
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: None,
                status: SessionStatus::Running,
                started_by: None,
                created_at: Some(0),
                last_active: Some(0),
                resume_attempts: 0,
                branch_owned: true,
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
}
