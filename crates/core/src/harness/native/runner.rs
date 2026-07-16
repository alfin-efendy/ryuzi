//! The native turn drain: one `run_turn` runs a prompt to completion, calling
//! the model, executing tools, and persisting + streaming everything through
//! the [`CoreEvent`] surface the rest of the engine consumes.

use super::agents::{Agent, AgentRegistry};
use super::commands::{CommandRegistry, ResolvedCommand};
use super::context_manager::{
    compaction::CompactionOutcome, is_context_overflow, truncate_for_context, ContextConfig,
    ContextManager,
};
use super::iteration_budget::{IterationBudget, PARENT_MAX_ITERS, SUBAGENT_MAX_ITERS};
use super::llm::LlmStream;
use super::permission::{evaluate, PermDecision};
use super::steer::SteerBuffer;
use super::tools::{
    BackgroundDispatch, MainAgentSpawner, MainDelegationResult, OutputCaps, SubagentSpawner,
    SubtaskResult, SubtaskSpec, SubtaskStatus, ToolCtx, ToolRegistry,
};
use super::{context, delegation, summary_budget, NATIVE_ID};
use crate::approval::ApprovalHub;
use crate::delegation::{RunHandle, SubagentRunRequest};
use crate::domain::{CoreEvent, NewMessage, PermMode, SessionKind};
use crate::harness::TurnPrompt;
use crate::llm_router::client::MessageStreamEvent;
use crate::llm_router::model_effort::TurnEffortPolicy;
use crate::llm_router::provenance::{
    LlmRequest, LlmRequestMetadata, RouteObservationContext, RouteSelection, RoutedStream,
};
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
/// Cap on the `tool.after` hook payload's result/output text — the tool's
/// own `for_model` text is already model-facing (not raw secret material),
/// but an external hook script is a different trust boundary than the LLM,
/// so the observational payload still gets a hard size ceiling.
const TOOL_AFTER_OUTPUT_BYTES: usize = 2_000;

/// Prefix of the `💾 Self-improvement review: …` notice the review fork
/// (Phase 4 Task 9) persists into the PARENT transcript when a learning row
/// finishes. Shared here — not owned by the review fork — so
/// `NativeHarness::start_session`'s best-effort nudge-state hydration
/// (`native/mod.rs`) can find the most recent review notice and count only
/// the user turns since it, without duplicating the literal string.
pub const SELF_IMPROVEMENT_NOTICE_PREFIX: &str = "💾 Self-improvement review";

/// Per-session nudge counters (Phase 4 §7.2), shared via `Arc` across a
/// session's whole lifetime — including every sub-agent it spawns, since
/// `RunnerDeps::clone()` copies the `Arc` pointer, not the state (a
/// delegated child's tool calls still count toward `skill_iters`; only the
/// TOP-LEVEL end_turn seam reads/resets these, gated on `DisplayMode::text()`
/// so a sub-agent's own end_turn — or a future review fork's, once Task 9
/// gives it a fresh `NudgeState` — never fires a nudge).
#[derive(Default)]
pub struct NudgeState {
    /// User turns completed since the last memory-review nudge fired.
    pub user_turns: std::sync::atomic::AtomicUsize,
    /// Tool-dispatch iterations since the last `skill_manage` call.
    pub skill_iters: std::sync::atomic::AtomicUsize,
}

/// The self-contained review-fork payload captured at finalize time. Byte-
/// for-byte replayable prefix (§7.2 prompt-cache parity): `system` +
/// `tool_defs` + `messages` are exactly what the parent's just-finished turn
/// sent (plus its own committed response), so the fork rides the warm cache.
/// `model` pins the parent's model — if `auxiliary.review.model` differs the
/// worker collapses history instead (Task 9).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LearningPayload {
    pub review_kind: String, // "memory" | "skill" | "combined"
    pub parent_session_pk: String,
    pub model: String,
    pub supports_prompt_cache: bool,
    pub system: String,
    pub tool_defs: Vec<Value>,
    pub messages: Vec<Value>,
}

/// Everything one native session needs to run turns. Built by
/// [`super::NativeHarness::start_session`]. Cloneable so a sub-agent spawner
/// can carry a copy.
#[derive(Clone)]
pub struct RunnerDeps {
    pub session_pk: String,
    /// Immutable primary identity and root run for this dispatched turn.
    pub primary_agent: Arc<crate::agents::types::AgentSnapshot>,
    pub run_id: String,
    /// The persisted primary root that owns all descendant rail outcomes.
    /// This remains stable while `run_id` changes for delegated main profiles
    /// and ephemeral sub-agent loops.
    pub root_run_id: String,
    pub delegation: Arc<crate::delegation::DelegationRuntime>,
    /// A child harness must not replay the root transcript or append provider
    /// turns to it; its result is conveyed only through its run-scoped rows.
    pub isolated_target: bool,
    /// Main agent whose isolated knowledge bundle owns this session's memory.
    pub main_agent_id: String,
    /// Durable learning queue shared with the daemon worker.
    pub learning_queue: Arc<crate::agents::learning_queue::LearningQueue>,
    /// Per-agent knowledge bundles used to construct isolated target memory.
    pub agent_knowledge: Arc<crate::agents::knowledge::AgentKnowledgeStore>,
    /// The session's kind (`Project`, `Chat`, `Worker`, `Review`), mirroring
    /// `SessionCtx::kind`. Consulted by `visible_tool_defs` to schema-gate
    pub kind: SessionKind,
    pub work_dir: PathBuf,
    /// Session attachments folder (second read root for the `read` tool).
    pub attachments_dir: Option<PathBuf>,
    /// Plugin-bundled skill directories folded in beside the worktree/global
    /// ones (see `crate::plugins::PluginHost::enabled_skill_dirs`).
    pub extra_skill_dirs: Vec<PathBuf>,
    /// Live handle to the daemon's extension host (Track D), threaded
    /// straight from `SessionCtx::extension_events` at session start — see
    /// that field's doc. `None` in the common case (no extensions spawned)
    /// and in every bare test `RunnerDeps`.
    pub extension_events: Option<Arc<dyn crate::plugins::extension::ExtensionEvents>>,
    pub model: Option<String>,
    /// Immutable effort/capability snapshot for the current turn.
    pub turn_effort_policy: Arc<TurnEffortPolicy>,
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
    /// Observational UI automation sink. It is deliberately separate from
    /// native script/extension hook dispatch, so it can never gate a tool.
    pub automation_events: Option<Arc<dyn crate::automation::AutomationEventSink>>,
    pub llm: Arc<dyn LlmStream>,
    pub tools: Arc<ToolRegistry>,
    /// The selected primary agent for this session.
    pub agent: Agent,
    /// Available agents (for sub-agent spawning).
    pub agents: Arc<AgentRegistry>,
    /// Available slash commands.
    pub commands: Arc<CommandRegistry>,
    /// Names of skills the durable primary profile permits. `None` leaves the
    /// native runtime's normal unrestricted discovery in place. Subagents reset
    /// this to `None` so their established unrestricted behavior remains.
    pub allowed_skills: Option<Vec<String>>,
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
    /// Shared async-delegation capacity gate (spec §6.2), from `SessionCtx`.
    pub background: Arc<super::background::BackgroundRegistry>,
    /// Curated app-control facade, present only for top-level interactive
    /// sessions (set by the control plane). Cloned per turn like the rest of
    /// `RunnerDeps`; the underlying `Arc` is shared.
    pub app_control: Option<Arc<dyn super::tools::AppControl>>,
    /// Nudge counters for the background learning loop (Phase 4 §7.2).
    /// Hydrated once in `NativeHarness::start_session`; shared (same `Arc`)
    /// with every sub-agent this session spawns.
    pub nudge: Arc<NudgeState>,
    /// Task 9 cache-parity override: when `Some`, `drive()` advertises THESE
    /// tool definitions verbatim instead of filtering `tools.definitions()`
    /// by `agent.tools` — the review fork must send the parent's exact
    /// captured `tool_defs` for the provider cache to hit, while dispatch
    /// (`run_tool_call`'s `agent.tools.allows` check) still enforces the
    /// fork's real whitelist regardless of what's advertised. `None` for
    /// every non-review turn (parent and sub-agent).
    pub review_tool_defs: Option<Vec<Value>>,
    /// Per-session set of deferred tools the model has loaded via `load_tools`
    /// (Phase 2 lazy tools). `Some` on primary sessions (lazy advertising on);
    /// `None` for sub-agents and review forks, which keep the eager filtered
    /// set. `BTreeSet` order keeps the advertised tools array deterministic so
    /// the prompt cache holds across turns with an unchanged set.
    pub activated_tools:
        Option<std::sync::Arc<tokio::sync::Mutex<std::collections::BTreeSet<String>>>>,
    /// Which actor is driving this session's tool calls (Phase 4 §7) —
    /// threaded into every `ToolCtx` this session's `run_tool_call` builds.
    /// `User` for ordinary interactive sessions; the background review fork
    /// (Task 9) sets `BackgroundReview` so Task 6's skill-write guard applies.
    pub write_origin: crate::domain::WriteOrigin,
    /// Profiles the runner can delegate to, rendered into the system prompt so
    /// `delegate_agent` has a current, executable target catalog.
    pub delegation_catalog: Vec<(String, String, String)>,
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

/// Normal-turn metadata that affects runtime execution without changing the
/// prompt text persisted or sent to the model.
#[derive(Debug, Clone, Default)]
struct TurnOptions {
    subtask: bool,
}

/// Return the resolved command's normal-turn runtime metadata. Kept separate
/// from prompt expansion so the flag cannot leak into model-visible text.
fn turn_options(command: &super::commands::ResolvedCommand) -> TurnOptions {
    TurnOptions {
        subtask: command.subtask,
    }
}

async fn max_provider_turns(deps: &RunnerDeps, options: &TurnOptions) -> usize {
    if options.subtask {
        SUBAGENT_MAX_ITERS
    } else {
        crate::settings::usize_setting(&deps.store, "agent.max_provider_turns", PARENT_MAX_ITERS)
            .await
    }
}

async fn command_root(deps: &RunnerDeps) -> PathBuf {
    let Some(project_id) = deps.project_id.as_deref() else {
        return deps.work_dir.clone();
    };
    match deps.store.get_project(project_id).await {
        Ok(Some(project)) => PathBuf::from(project.workdir),
        Ok(None) => deps.work_dir.clone(),
        Err(error) => {
            tracing::warn!(project_id, %error, "native: falling back to active worktree for command root");
            deps.work_dir.clone()
        }
    }
}

/// Resolve a slash command from its current project root. Agent overrides use
/// the matching root's registry; absent command agent metadata leaves the
/// session's active-worktree agent unchanged.
async fn resolve_slash_command(
    deps: &RunnerDeps,
    input: &str,
) -> Option<(ResolvedCommand, Option<Agent>)> {
    let root = command_root(deps).await;
    let commands = CommandRegistry::load(&root);
    let agents = AgentRegistry::load(&root);
    let resolved = commands.resolve(input)?;
    let agent = resolved.agent.as_deref().and_then(|name| agents.get(name));
    Some((resolved, agent))
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
    let trimmed = prompt.display.trim();
    let manual_compact = trimmed == "/compact" || trimmed.starts_with("/compact ");
    let force_subtask = prompt.force_subtask;

    // Slash-command resolution on the raw user text. Reload only for a
    // command-shaped prompt so project command CRUD becomes visible to a live
    // session without adding filesystem work to ordinary turns. When the
    // owning project is reachable, its canonical workdir is the command root;
    // chat/bare or deleted-project sessions fall back to the active worktree.
    // Command metadata stays outside the expanded prompt and is applied only
    // to this turn.
    let (agent_text, agent, command_model, mut options) = if manual_compact {
        (
            prompt.agent.clone(),
            deps.agent.clone(),
            None,
            TurnOptions::default(),
        )
    } else {
        let resolved = if trimmed.starts_with('/') {
            resolve_slash_command(deps, &prompt.display).await
        } else {
            deps.commands
                .resolve(&prompt.display)
                .map(|resolved| (resolved, None))
        };
        match resolved {
            Some((resolved, command_agent)) => {
                let agent = command_agent.unwrap_or_else(|| deps.agent.clone());
                let options = turn_options(&resolved);
                (
                    merge_agent_prompt_suffix(resolved.prompt, &prompt),
                    agent,
                    resolved.model,
                    options,
                )
            }
            None => (
                prompt.agent.clone(),
                deps.agent.clone(),
                None,
                TurnOptions::default(),
            ),
        }
    };
    // A caller-supplied override (currently: automation Hook agent runs)
    // wins over slash-command resolution — the hook's `subtask` field must
    // reach the same runtime budget a command's `subtask: true` frontmatter
    // does, regardless of whether the prompt happened to start with `/`.
    if let Some(force_subtask) = force_subtask {
        options.subtask = force_subtask;
    }

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

    // Complete per-turn configuration snapshot: re-read the project's pinned
    // model/effort, configured and provider defaults, eligible surfaces, and
    // ModelMeta. Everything below — request bodies, compaction, title
    // generation, and the sub-agent spawner — shares this immutable snapshot;
    // the original `deps` is never mutated, so in-flight turns and running
    // subagents keep the configuration they started with.
    let turn_deps = refresh_turn_configuration(deps, command_model).await;
    let deps = &turn_deps;

    // /compact is an action, but it still snapshots the same complete turn
    // configuration as every other turn before making its utility call.
    if manual_compact {
        return run_manual_compact(deps).await;
    }

    // 2. Load history + context state and append the user turn.
    let cfg = ContextConfig::load(&deps.store, deps.meta.clone()).await;
    let mut cm = if deps.isolated_target {
        ContextManager::ephemeral(&deps.session_pk, cfg)
    } else {
        ContextManager::load(deps.store.clone(), &deps.session_pk, cfg).await?
    };
    // Seed the indicator immediately on resume, before any model call —
    // prefer the persisted last-known status (server truth) over the
    // reload estimate (spec §6.1/§10).
    if !deps.isolated_target {
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
                    cache_creation_tokens: 0,
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
    }
    cm.append_user(user_content_blocks(&prompt.blocks, &agent_text))
        .await?;

    // 3. Drive the loop. Isolated complete-profile children retain `task`
    // execution when it is advertised, but their RunnerSpawner still strips
    // attachments, app control, and persistent memory in deps_for_subagent.
    let spawn = Some(Arc::new(RunnerSpawner {
        deps: deps.clone(),
        cancel: cancel.clone(),
        depth: 0,
        parent_run_id: deps.run_id.clone(),
    }) as Arc<dyn SubagentSpawner>);
    // Seed the parent turn-cap from the `agent.max_provider_turns` setting,
    // defaulting to Phase 2's raised ceiling (PARENT_MAX_ITERS). This is what
    // makes the setting meaningful under the IterationBudget model: drive()'s
    // `while budget.try_consume()` loop caps at exactly this many provider
    // turns per window, and each auto-continue re-grants a fresh window of the
    // same size (drive() re-reads the setting for that grant).
    let max_provider_turns = max_provider_turns(deps, &options).await;
    let budget = IterationBudget::new(max_provider_turns);
    drive(
        deps,
        &agent,
        &mut cm,
        &cancel,
        spawn,
        DisplayMode::Full,
        &budget,
    )
    .await?;

    // 4. Best-effort: give a fresh session a generated title.
    if !deps.isolated_target {
        maybe_generate_title(deps, &prompt.display).await;
    }
    Ok(())
}

/// Manual /compact: persist the user's row, compact the session history, and
/// record a notice row. No model turn runs.
async fn run_manual_compact(deps: &RunnerDeps) -> anyhow::Result<()> {
    let cfg = ContextConfig::load(&deps.store, deps.meta.clone()).await;
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
    match cm
        .compact(
            &deps.llm,
            &cmodel,
            "manual",
            deps.turn_effort_policy.clone(),
        )
        .await
    {
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
async fn refresh_turn_configuration(
    deps: &RunnerDeps,
    command_model: Option<String>,
) -> RunnerDeps {
    let scheduler_override = scheduler_model_override(deps).await;
    let project_pin = project_pinned_model(deps).await;
    let session_pin = if project_pin.is_none() {
        chat_session_pinned_model(&deps.store, &deps.session_pk).await
    } else {
        None
    };
    let has_command_model = command_model
        .as_deref()
        .is_some_and(|model| !model.trim().is_empty());
    let pinned = command_model
        .filter(|model| !model.trim().is_empty())
        .or(scheduler_override.clone())
        .or_else(|| deps.model.clone())
        .or_else(|| project_pin.clone().flatten())
        .or(session_pin.clone());
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
    let model = turn.model.as_deref().unwrap_or("");
    let primary_model = agent_model_name(&turn.primary_agent.profile.model);
    if has_command_model
        || scheduler_override.is_some()
        || project_pin.is_some()
        || session_pin.is_some()
        || turn.model != deps.model
        || primary_model.as_deref() == Some(model)
    {
        turn.meta = crate::llm_router::model_meta::resolve(&turn.store, model).await;
    }
    let policy = if let Some(project_id) = turn.project_id.as_deref() {
        crate::llm_router::model_effort::build_turn_effort_policy(&turn.store, project_id, model)
            .await
    } else {
        crate::llm_router::model_effort::build_session_effort_policy(
            &turn.store,
            &turn.session_pk,
            model,
        )
        .await
    };
    if let Ok(mut policy) = policy {
        policy.caller_override = if turn.agent.mode.is_subagent() {
            turn.turn_effort_policy.caller_override.clone()
        } else {
            policy
                .caller_override
                .clone()
                .or_else(|| turn.turn_effort_policy.caller_override.clone())
                .or_else(|| agent_effort(&turn.primary_agent.profile.model))
        };
        turn.turn_effort_policy = Arc::new(policy);
    }
    turn
}

/// `Some(project.model)` when the session's project row is reachable — the
/// inner Option is the pin itself, which may legitimately be unset. `None`
/// when there is no session/project row to read, or the session has no
/// bound project (chat-first sessions).
async fn scheduler_model_override(deps: &RunnerDeps) -> Option<String> {
    let session = deps
        .store
        .get_session(&deps.session_pk)
        .await
        .ok()
        .flatten()?;
    if session.started_by.as_deref() != Some("scheduler") {
        return None;
    }
    deps.store
        .get_session_runtime_settings(&deps.session_pk)
        .await
        .ok()
        .flatten()
        .and_then(|runtime| runtime.model)
}

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

async fn chat_session_pinned_model(store: &Store, session_pk: &str) -> Option<String> {
    let session = store.get_session(session_pk).await.ok().flatten()?;
    if session.kind != crate::domain::SessionKind::Chat {
        return None;
    }
    store
        .get_session_runtime_settings(session_pk)
        .await
        .ok()
        .flatten()
        .and_then(|runtime| runtime.model)
}

fn agent_model_name(model: &crate::agents::types::AgentModel) -> Option<String> {
    match model {
        crate::agents::types::AgentModel::Concrete { name, .. } => Some(name.clone()),
        crate::agents::types::AgentModel::Route { route } => Some(route.clone()),
    }
}

fn agent_effort(model: &crate::agents::types::AgentModel) -> Option<String> {
    match model {
        crate::agents::types::AgentModel::Concrete { effort, .. } => effort.clone(),
        crate::agents::types::AgentModel::Route { .. } => None,
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
    let Ok(title) =
        super::llm::collect_text(&deps.llm, body, deps.turn_effort_policy.clone()).await
    else {
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
    /// Background review fork (Task 9): nothing but its own tool_call rows
    /// (on the review session, never the parent) — no text/thinking/notice/
    /// context-usage display, no auto-continue, no compaction, and —
    /// crucially — `display.text()` gates `maybe_enqueue_review`'s nudge
    /// trigger, so a review fork can never recursively enqueue another
    /// review of itself.
    Silent,
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
            DisplayMode::Full | DisplayMode::Silent => None,
        }
    }
}

/// Hermes' verbatim nudge for the post-exhaustion summary call: asks for a
/// final answer without inviting another round of tool calls.
const BUDGET_EXHAUSTED_PROMPT: &str = "You've reached the maximum number of \
    tool-calling iterations allowed. Please provide a final response \
    summarizing what you've found and accomplished so far, without calling \
    any more tools.";

/// Synthetic meta-tool name. `load_tools` is NOT a registry tool — its
/// definition is injected here and its call is intercepted in `run_tool_call`.
pub(crate) const LOAD_TOOLS_NAME: &str = "load_tools";

/// Always-advertised built-ins. Everything else (niche built-ins + all MCP /
/// extension tools) is deferred until the model loads it via `load_tools`.
const HOT_TOOLS: &[&str] = &[
    "read",
    "ls",
    "glob",
    "grep",
    "bash",
    "edit",
    "write",
    "todowrite",
    "todoread",
    "skill",
    "task",
];

fn is_hot(name: &str) -> bool {
    HOT_TOOLS.contains(&name)
}

/// The synthetic `load_tools` definition, whose description carries the current
/// deferred index (name + one-line summary) so the model knows what it can load.
fn load_tools_definition(deferred_index: &[(String, String)]) -> Value {
    let mut description = String::from(
        "Load additional tools into this session by name so you can call them.          Only the tools listed below can be loaded; call this with the exact          names you need, then use them on your next step.

Available to load:",
    );
    for (name, summary) in deferred_index {
        description.push_str(&format!(
            "
- {name}: {summary}"
        ));
    }
    json!({
        "name": LOAD_TOOLS_NAME,
        "description": description,
        "input_schema": {
            "type": "object",
            "properties": {
                "names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Exact tool names to load, taken from the list in this description."
                }
            },
            "required": ["names"]
        }
    })
}

/// Tool definitions advertised to the model this turn. `activated = None`
/// preserves eager filtered advertisement for subagents/review workers; a
/// primary session advertises the hot core plus loaded deferred tools.
fn visible_tool_defs(
    tools: &ToolRegistry,
    agent: &Agent,
    activated: Option<&std::collections::BTreeSet<String>>,
) -> Vec<Value> {
    let allowed = |name: &str| agent.tools.allows(name);

    let Some(activated) = activated else {
        return tools
            .definitions()
            .into_iter()
            .filter(|definition| {
                definition
                    .get("name")
                    .and_then(|name| name.as_str())
                    .map(&allowed)
                    .unwrap_or(false)
            })
            .collect();
    };

    let mut advertised = Vec::new();
    let mut deferred_index = Vec::new();
    for definition in tools.definitions() {
        let Some(name) = definition.get("name").and_then(|name| name.as_str()) else {
            continue;
        };
        if !allowed(name) {
            continue;
        }
        if is_hot(name) || activated.contains(name) {
            advertised.push(definition);
        } else {
            let summary = definition
                .get("description")
                .and_then(|description| description.as_str())
                .and_then(|description| description.lines().next())
                .unwrap_or("")
                .to_string();
            deferred_index.push((name.to_string(), summary));
        }
    }
    advertised.push(load_tools_definition(&deferred_index));
    advertised
}

/// The tool definitions to send this provider turn: the review fork's captured
/// set verbatim (cache parity), else an activation-aware snapshot.
async fn current_tool_defs(deps: &RunnerDeps, agent: &Agent) -> Vec<Value> {
    if let Some(captured) = &deps.review_tool_defs {
        return captured.clone();
    }
    let activated = match &deps.activated_tools {
        Some(activated) => Some(activated.lock().await.clone()),
        None => None,
    };
    visible_tool_defs(&deps.tools, agent, activated.as_ref())
}

fn with_delegation_catalog(system: String, catalog: &[(String, String, String)]) -> String {
    if catalog.is_empty() {
        return system;
    }
    let entries = catalog
        .iter()
        .map(|(id, name, description)| format!("- `{id}` ({name}): {description}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{system}\n\nAvailable complete agent profiles for `delegate_agent`:\n{entries}")
}

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
    let mut system_breakdown: Option<Vec<(&'static str, u64)>> = None;
    let system = match &agent.prompt {
        Some(p) => p.clone(),
        None => {
            let memory = match deps.memory.as_ref() {
                Some(memory) => memory.snapshot().await?,
                None => None,
            };
            let t0 = std::time::Instant::now();
            let sections = context::build_sections(
                &deps.work_dir,
                &deps.extra_skill_dirs,
                memory.as_deref(),
                deps.allowed_skills.as_deref(),
            );
            system_breakdown = Some(context::breakdown_of(&sections));
            let text = context::join_sections(&sections);
            tracing::debug!(
                target: "ryuzi::context",
                elapsed_ms = t0.elapsed().as_millis() as u64,
                "native: system prompt assembled"
            );
            text
        }
    };
    let system = with_delegation_catalog(system, &deps.delegation_catalog);
    // Tools restricted to what this agent may use — UNLESS a review fork
    // (Task 9) supplies the parent's exact captured `tool_defs` for cache
    // parity. Advertising the full parent tool set here is safe even though
    // the review agent's `ToolFilter` only allows a few of them: dispatch
    // (`run_tool_call`, below) enforces the real whitelist at call time, so a
    // non-whitelisted call is refused, not merely hidden from the model.
    let tool_defs: Vec<Value> = match &deps.review_tool_defs {
        Some(captured) => captured.clone(),
        None => current_tool_defs(deps, agent).await,
    };
    let model = deps.model.clone().unwrap_or_default();
    let mut final_text = String::new();

    cm.set_baseline(&system, &tool_defs);
    if let Some(mut bd) = system_breakdown.take() {
        let tools_tokens: u64 = tool_defs
            .iter()
            .map(|t| serde_json::to_string(t).map(|s| s.len()).unwrap_or(0) as u64)
            .sum::<u64>()
            / 4;
        bd.push(("tools", tools_tokens));
        tracing::debug!(
            target: "ryuzi::context",
            breakdown = ?bd,
            baseline_tokens = cm.status().active_tokens,
            "native: context baseline breakdown"
        );
    }
    let settings_cap =
        crate::settings::usize_setting(&deps.store, "context.max_output_tokens", 1).await;
    // usize_setting floors at 1; treat 1 (the "unset" default) as no cap.
    let max_tokens = if settings_cap > 1 {
        (deps.meta.max_output_tokens as usize).min(settings_cap) as i64
    } else {
        deps.meta.max_output_tokens as i64
    };
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
            // Skipped for a review fork (`DisplayMode::Silent`, Task 9): its
            // budget is tiny (16) and its whole point is to replay the
            // parent's captured prefix byte-for-byte — compacting it would
            // both defeat cache parity and is never needed at this size.
            if !matches!(display, DisplayMode::Silent) && cm.status().needs_compaction {
                let trigger = if provider_turn == 0 {
                    "pre_turn"
                } else {
                    "mid_turn"
                };
                let cmodel = super::llm::aux_model(&deps.store, "compaction", &model).await;
                match cm
                    .compact(&deps.llm, &cmodel, trigger, deps.turn_effort_policy.clone())
                    .await
                {
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
            let tool_defs = current_tool_defs(deps, agent).await;
            let body = json!({
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
            let observation = display.text().then(|| RouteObservationContext {
                session_pk: deps.session_pk.clone(),
            });
            let request = LlmRequest {
                body,
                metadata: LlmRequestMetadata {
                    effort_policy: deps.turn_effort_policy.clone(),
                    observation: observation.clone(),
                },
            };
            let ttft_start = std::time::Instant::now();
            let mut ttft_logged = false;
            let RoutedStream {
                selection,
                events: mut rx,
            } = match deps.llm.stream(request).await {
                Ok(routed) => routed,
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
            if let Some(context) = observation.as_ref() {
                observe_route_selection(deps, context, &selection).await;
            }
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
                if !ttft_logged {
                    ttft_logged = true;
                    tracing::debug!(
                        target: "ryuzi::context",
                        ttft_ms = ttft_start.elapsed().as_millis() as u64,
                        "native: first stream event received"
                    );
                }
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
                // Nudge trigger (Phase 4 §7.2): only the top-level, display-
                // visible drive() ever nudges. `display.text()` is false for
                // sub-agents (`ToolsOnly`) and will be false for Task 9's
                // review fork (`Silent`) too — both share this `drive()` loop,
                // so gating here (rather than trusting callers not to spawn a
                // learning row) is what actually prevents a sub-agent or a
                // review fork from recursively enqueueing another review.
                if display.text() {
                    maybe_enqueue_review(deps, &system, &tool_defs, cm).await;
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
                // "Tool iterations since last skill_manage" (§7.2): every
                // dispatched tool other than `skill_manage` advances the
                // skill-nudge counter; `skill_manage` itself resets it — the
                // agent just acted on the skill signal, so the pressure that
                // built up is spent. Shared across sub-agents (same `Arc`),
                // mirroring `user_turns` — see `NudgeState`'s doc comment.
                use std::sync::atomic::Ordering::Relaxed;
                if t.name == "skill_manage" {
                    deps.nudge.skill_iters.store(0, Relaxed);
                } else {
                    deps.nudge.skill_iters.fetch_add(1, Relaxed);
                }
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
        if let Ok(text) =
            super::llm::collect_text(&deps.llm, body, deps.turn_effort_policy.clone()).await
        {
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

/// The nudge trigger (Phase 4 §7.2): called only from the top-level end_turn
/// seam (see `drive()`, gated on `display.text()`). Advances `user_turns`;
/// when either the memory or skill threshold is crossed, captures the exact
/// `system` / `tool_defs` / `messages_for_request()` prefix this turn just
/// used — the cache-parity payload Task 9's review fork replays — and enqueues
/// it in the durable learning queue for this session's executing main agent.
async fn maybe_enqueue_review(
    deps: &RunnerDeps,
    system: &str,
    tool_defs: &[Value],
    cm: &ContextManager,
) {
    use std::sync::atomic::Ordering::Relaxed;
    let mem_interval =
        crate::settings::usize_setting(&deps.store, "memory.nudge_interval", 10).await;
    let skill_interval =
        crate::settings::usize_setting(&deps.store, "skills.creation_nudge_interval", 10).await;
    let turns = deps.nudge.user_turns.fetch_add(1, Relaxed) + 1;
    let iters = deps.nudge.skill_iters.load(Relaxed);
    let mem_due = turns >= mem_interval;
    let skill_due = iters >= skill_interval;
    if !mem_due && !skill_due {
        return;
    }
    let review_kind = match (mem_due, skill_due) {
        (true, true) => "combined",
        (true, false) => "memory",
        _ => "skill",
    };
    let payload = LearningPayload {
        review_kind: review_kind.into(),
        parent_session_pk: deps.session_pk.clone(),
        model: deps.model.clone().unwrap_or_default(),
        supports_prompt_cache: deps.meta.supports_prompt_cache,
        system: system.to_string(),
        tool_defs: tool_defs.to_vec(),
        messages: cm.messages_for_request(),
    };
    let serialized = serde_json::to_string(&payload).unwrap_or_default();
    let review = crate::agents::learning_queue::ReviewEvent {
        title: format!("{} review", payload.review_kind),
        description: format!("Learning review from session {}", deps.session_pk),
        body: serialized,
        tags: vec![payload.review_kind.clone()],
    };
    if let Err(error) = deps
        .learning_queue
        .enqueue(
            &deps.main_agent_id,
            crate::agents::learning_queue::LearningEventPayload::Review(review),
        )
        .await
    {
        tracing::warn!(
            agent_id = %deps.main_agent_id,
            "failed to enqueue durable learning review: {error}"
        );
    }
    if mem_due {
        deps.nudge.user_turns.store(0, Relaxed);
    }
    if skill_due {
        deps.nudge.skill_iters.store(0, Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Review fork (Phase 4 Task 9): consumes a `LearningPayload` captured by
// `maybe_enqueue_review` above and replays it as a byte-identical-prefix,
// tool-whitelisted turn. `ControlPlane::run_review_fork` (control/
// lifecycle.rs) owns session bookkeeping (insert/end the `kind='review'`
// row, resolve the model, build `RunnerDeps`) and calls [`drive_review`].
// ---------------------------------------------------------------------------

/// Tools the review fork's dispatch gate allows, in addition to whatever
/// `tool_defs` it advertises for cache parity (Task 9). Enforced by
/// `run_tool_call`'s `agent.tools.allows` check — a call to anything else is
/// refused with a tool_result error, never executed.
pub const REVIEW_TOOL_WHITELIST: &[&str] = &["memory", "skill", "skill_manage"];

/// Hermes-agent's memory review prompt, ported verbatim (Task 9 §3).
pub const MEMORY_REVIEW_PROMPT: &str = "\
Take a moment to review the conversation above for durable facts worth \
remembering. Use the `memory` tool to add or consolidate entries. Only record \
things that will still be true and useful next week: user preferences and \
style (user scope), environment and conventions (global scope), or codebase \
facts (project scope). Do not record task state, transient details, or secrets. \
Prefer editing an existing entry over adding a near-duplicate. If nothing is \
worth remembering, do nothing and end your turn.";

/// Hermes-agent's skill review prompt, ported verbatim (Task 9 §3).
pub const SKILL_REVIEW_PROMPT: &str = "\
Review the conversation above for a reusable procedure worth capturing as a \
skill. A good skill is a repeatable, generalizable workflow — not a one-off. \
Preference ladder: (1) improve an existing skill with `skill_manage` patch/edit \
before (2) creating a new one. Never capture user-specific secrets or a single \
task's state (anti-capture). You MUST `skill` (view) a skill before editing it. \
If nothing generalizes, do nothing and end your turn.";

/// Appended to whichever review prompt(s) run, teaching the enforced
/// whitelist so the model doesn't waste a turn probing for a denied tool.
pub const REVIEW_TOOL_RESTRICTION_NOTE: &str = "\
For this review you may ONLY use the tools `memory`, `skill`, and \
`skill_manage`. All other tools are disabled and will return an error.";

/// The final user turn appended after the replayed/digested prefix:
/// `review_kind`'s prompt(s) plus the tool-restriction note. `"combined"`
/// (and any other value — defensively, the safest choice) runs both.
pub(crate) fn review_prompt_text(review_kind: &str) -> String {
    let body = match review_kind {
        "memory" => MEMORY_REVIEW_PROMPT.to_string(),
        "skill" => SKILL_REVIEW_PROMPT.to_string(),
        _ => format!("{MEMORY_REVIEW_PROMPT}\n\n{SKILL_REVIEW_PROMPT}"),
    };
    format!("{body}\n\n{REVIEW_TOOL_RESTRICTION_NOTE}")
}

/// The synthetic primary agent a review fork drives as — `prompt` carries the
/// captured (or digested) RAW system string verbatim (`drive()` uses it as-is
/// when `Some`, see the `system` binding at the top of `drive()`), and
/// `tools` is the dispatch-time whitelist (`REVIEW_TOOL_WHITELIST`) that
/// `run_tool_call`'s `agent.tools.allows` check enforces regardless of what
/// `RunnerDeps::review_tool_defs` advertises.
pub(crate) fn review_agent(system: String) -> Agent {
    Agent {
        name: "review".into(),
        description: "Background self-improvement review fork".into(),
        mode: super::agents::AgentMode::Primary,
        prompt: Some(system),
        tools: super::agents::ToolFilter::Only(
            REVIEW_TOOL_WHITELIST
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ),
        permission_rules: Vec::new(),
        can_delegate: false,
        builtin: true,
    }
}

/// Thin `drive()` wrapper fixing the review fork's shape: no sub-agent
/// spawner, `DisplayMode::Silent` (no text/notice/context-usage display, no
/// auto-continue, no compaction, and — via `display.text()` — no recursive
/// nudge), and a small fixed budget. `deps`/`agent`/`cm` are caller-built
/// (`ControlPlane::run_review_fork`) so this stays a pure driving seam,
/// testable with a scripted `LlmStream` and no `ControlPlane` at all.
pub(crate) async fn drive_review(
    deps: &RunnerDeps,
    agent: &Agent,
    cm: &mut ContextManager,
    cancel: &CancellationToken,
) -> anyhow::Result<String> {
    const REVIEW_MAX_ITERS: usize = 16;
    drive(
        deps,
        agent,
        cm,
        cancel,
        None,
        DisplayMode::Silent,
        &IterationBudget::new(REVIEW_MAX_ITERS),
    )
    .await
}

/// Tools delegated children may never use regardless of filters. `task` is
/// re-armed for agents permitted to delegate; `memory` never is —
/// sub-agents run memoryless, mirroring hermes-agent's `skip_memory`. The todo
/// tools are blocked because the list is keyed by the parent's session_pk: a
/// child's `todowrite` would silently clobber the user-visible plan.
/// `skill_manage` (Phase 4 Task 6) writes filesystem skills gated by an
/// origin × provenance guard — a primary/review-fork capability, never a
/// sub-agent's; `skill` (read-only recall) stays available to children.
/// App-control tools (`crate::harness::native::tools::APP_TOOLS`, Phase 6
/// §9.1) never reach delegated children either — `ctx.app` is `None` in a
/// sub-agent's own `ToolCtx` regardless, so this is belt-and-suspenders: it
/// also keeps them out of the tool definitions advertised to the model.
const SUBAGENT_BLOCKLIST: &[&str] = &[
    "task",
    "memory",
    "todowrite",
    "todoread",
    "skill_manage",
    "app_jobs",
    "app_projects",
    "clarify",
];
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

/// The `max_spawn_depth` setting controls how many delegation hops a child may make.
async fn max_spawn_depth(store: &Store) -> u8 {
    crate::settings::usize_setting(store, "max_spawn_depth", 2).await as u8
}

/// Build one memoryless task child's dependencies from the current shared
/// subagent configuration. The parent contributes only session-scoped services
/// and isolation boundaries; its model, metadata, and effort policy never
/// select the child model.
async fn deps_for_subagent(deps: &RunnerDeps) -> anyhow::Result<RunnerDeps> {
    let shared_model = deps.delegation.registry_snapshot().await.subagent_model;
    let model = super::resolve_native_model(&deps.store, agent_model_name(&shared_model)).await;
    let model_name = model.as_deref().unwrap_or("");
    let mut effort_policy =
        crate::llm_router::model_effort::build_utility_effort_policy(&deps.store, model_name)
            .await?;
    effort_policy.caller_override = agent_effort(&shared_model);
    let meta = crate::llm_router::model_meta::resolve(&deps.store, model_name).await;

    Ok(RunnerDeps {
        session_pk: deps.session_pk.clone(),
        primary_agent: deps.primary_agent.clone(),
        run_id: deps.run_id.clone(),
        root_run_id: deps.root_run_id.clone(),
        delegation: deps.delegation.clone(),
        isolated_target: deps.isolated_target,
        main_agent_id: deps.main_agent_id.clone(),
        learning_queue: deps.learning_queue.clone(),
        agent_knowledge: deps.agent_knowledge.clone(),
        kind: deps.kind,
        work_dir: deps.work_dir.clone(),
        attachments_dir: None,
        extra_skill_dirs: deps.extra_skill_dirs.clone(),
        extension_events: deps.extension_events.clone(),
        model,
        turn_effort_policy: Arc::new(effort_policy),
        meta,
        perm_mode: deps.perm_mode.clone(),
        project_id: deps.project_id.clone(),
        perm_overrides: deps.perm_overrides.clone(),
        store: deps.store.clone(),
        events: deps.events.clone(),
        approvals: deps.approvals.clone(),
        automation_events: deps.automation_events.clone(),
        llm: deps.llm.clone(),
        tools: deps.tools.clone(),
        agent: deps.agent.clone(),
        agents: deps.agents.clone(),
        commands: deps.commands.clone(),
        allowed_skills: None,
        memory: None,
        snapshots: deps.snapshots.clone(),
        steer: deps.steer.clone(),
        background: deps.background.clone(),
        app_control: None,
        nudge: deps.nudge.clone(),
        review_tool_defs: None,
        activated_tools: None,
        write_origin: deps.write_origin,
        delegation_catalog: deps.delegation_catalog.clone(),
    })
}

/// A [`SubagentSpawner`] backed by the runner: runs sub-agents in ephemeral
/// (unpersisted-history) sub-loops and returns their final texts. `depth` is
/// how many delegation hops separate this spawner from the primary agent.
struct RunnerSpawner {
    deps: RunnerDeps,
    cancel: CancellationToken,
    depth: u8,
    /// The durable run that owns children spawned through this particular
    /// `task` capability. Root turns use the primary run; each child spawner
    /// replaces this with its own child run before it can recurse.
    parent_run_id: String,
}

/// Add the fixed delegation capabilities that only a delegated complete-profile
/// child receives. Its configured profile filter remains authoritative for every
/// other tool; the registry check keeps the advertised capabilities dispatchable.
fn effective_delegated_main_child_filter(
    filter: super::agents::ToolFilter,
    names: &[String],
) -> super::agents::ToolFilter {
    match filter {
        super::agents::ToolFilter::All => super::agents::ToolFilter::All,
        super::agents::ToolFilter::Only(mut allowed) => {
            // Isolated main profiles advertise `task` when the delegation
            // contract requires it, and their terminal turn receives the
            // constrained RunnerSpawner above.
            for delegation_tool in ["task", "delegate_agent"] {
                if names.iter().any(|name| name == delegation_tool)
                    && !allowed.iter().any(|name| name == delegation_tool)
                {
                    allowed.push(delegation_tool.to_string());
                }
            }
            allowed.sort();
            super::agents::ToolFilter::Only(allowed)
        }
    }
}

/// Runs complete durable main profiles in isolated child harnesses. The child
/// receives its own immutable profile through `RunHandle.agent_snapshot`; it
/// never receives parent attachments or persistent parent memory.
struct RunnerMainAgentSpawner {
    deps: RunnerDeps,
}

impl RunnerMainAgentSpawner {
    async fn run_child(
        &self,
        request: crate::delegation::MainDelegationRequest,
    ) -> MainDelegationResult {
        let background = request.background;
        let context = request.context.clone();
        let root_run_id = self.deps.root_run_id.clone();
        let child_run = match self.deps.delegation.queue_main(request).await {
            Ok(child) => child,
            Err(error) => {
                return MainDelegationResult {
                    run_id: String::new(),
                    agent_id: String::new(),
                    status: SubtaskStatus::Error,
                    report: error.to_string(),
                };
            }
        };
        let run_id = child_run.run.run_id.clone();
        let agent_id = child_run.run.executing_agent_id.clone().unwrap_or_default();
        if background {
            let cap =
                crate::settings::usize_setting(&self.deps.store, "max_concurrent_runs", 3).await;
            let Some(reservation) = self.deps.background.try_reserve(cap, &self.deps.session_pk)
            else {
                let _ = self
                    .deps
                    .delegation
                    .cancel_child(&self.deps.session_pk, &run_id)
                    .await;
                return MainDelegationResult {
                    run_id,
                    agent_id,
                    status: SubtaskStatus::Error,
                    report: format!(
                        "Async delegation capacity reached ({cap} running). Run this task synchronously."
                    ),
                };
            };
            let worker = Self {
                deps: self.deps.clone(),
            };
            let goal = child_run.run.task.clone();
            let child_run_id = child_run.run.run_id.clone();
            let session_pk = self.deps.session_pk.clone();
            tokio::spawn(async move {
                let reservation_cancel = reservation.token();
                let execution = worker.execute_child(child_run, context);
                tokio::pin!(execution);
                let result = tokio::select! {
                    _ = reservation_cancel.cancelled() => {
                        // The terminal transition awaits SQLite; release the
                        // capacity guard first so ending a session never holds
                        // a slot hostage on that I/O.
                        drop(reservation);
                        let _ = worker
                            .deps
                            .delegation
                            .cancel_child(&session_pk, &child_run_id)
                            .await;
                        return;
                    }
                    result = &mut execution => result,
                };
                if reservation_cancel.is_cancelled() {
                    return;
                }
                worker
                    .deliver_background_result(&root_run_id, &goal, &result)
                    .await;
            });
            return MainDelegationResult::completed(
                run_id,
                agent_id,
                "background delegation dispatched".to_string(),
            );
        }
        self.execute_child(child_run, context).await
    }

    async fn execute_child(
        &self,
        child: RunHandle,
        context: Option<String>,
    ) -> MainDelegationResult {
        let run_id = child.run.run_id.clone();
        let agent_id = child.run.executing_agent_id.clone().unwrap_or_default();
        let result = async {
            self.deps.delegation.mark_running(&run_id).await?;
            let snapshot = child
                .agent_snapshot
                .clone()
                .ok_or_else(|| anyhow::anyhow!("delegated agent snapshot is unavailable"))?;
            let mut child_deps = self.deps.clone();
            child_deps.run_id = run_id.clone();
            let primary_turn = super::primary_turn_config_with_tools(
                snapshot.clone(),
                run_id.clone(),
                child_deps.root_run_id.clone(),
                &child_deps.tools.names(),
            )?;
            child_deps.primary_agent = primary_turn.agent;
            child_deps.main_agent_id = snapshot.profile.id.clone();
            child_deps.isolated_target = true;
            child_deps.attachments_dir = None;
            child_deps.memory = Some(Arc::new(super::memory::MemoryStore::for_agent(
                child_deps.agent_knowledge.clone(),
                &snapshot.profile.id,
                child_deps.project_id.as_deref(),
            )?));
            // A delegated target may advertise its configured app tool but
            // never receives the parent's app-control facade to execute it.
            child_deps.app_control = None;
            child_deps.perm_overrides = Arc::new(std::sync::Mutex::new(Default::default()));
            child_deps.perm_mode = Arc::new(std::sync::Mutex::new(primary_turn.perm_mode));
            child_deps.model = primary_turn.model;
            child_deps.meta = crate::llm_router::model_meta::resolve(
                &child_deps.store,
                child_deps.model.as_deref().unwrap_or(""),
            )
            .await;
            let mut effort_policy = crate::llm_router::model_effort::build_utility_effort_policy(
                &child_deps.store,
                child_deps.model.as_deref().unwrap_or(""),
            )
            .await?;
            effort_policy.caller_override = primary_turn.effort;
            child_deps.turn_effort_policy = Arc::new(effort_policy);
            child_deps.allowed_skills = primary_turn.allowed_skills;
            child_deps.agent = primary_turn.agent_tools;
            child_deps.agent.tools = effective_delegated_main_child_filter(
                child_deps.agent.tools,
                &child_deps.tools.names(),
            );
            // The child's own catalog excludes ITS profile (not the
            // parent's) — a delegated agent must never be offered a
            // `delegate_agent` route back to the profile it's currently
            // executing as.
            child_deps.delegation_catalog = self
                .deps
                .delegation
                .delegate_catalog(&snapshot.profile.id)
                .await;
            let mut cm = ContextManager::ephemeral(
                &child_deps.session_pk,
                ContextConfig::with_meta(child_deps.meta.clone()),
            );
            let task = child.run.task.clone();
            let mut prompt = vec![json!({ "type": "text", "text": task })];
            if let Some(context) = context.filter(|context| !context.trim().is_empty()) {
                prompt.push(json!({ "type": "text", "text": context }));
            }
            cm.append_user(Value::Array(prompt)).await?;
            let turns = snapshot.profile.loop_settings.max_turns.max(1) as usize;
            let text = drive(
                &child_deps,
                &child_deps.agent.clone(),
                &mut cm,
                &child.cancel,
                Some(Arc::new(RunnerSpawner {
                    deps: child_deps.clone(),
                    cancel: child.cancel.clone(),
                    depth: 0,
                    parent_run_id: run_id.clone(),
                })),
                DisplayMode::ToolsOnly {
                    label: snapshot.profile.name.clone(),
                },
                &IterationBudget::new(turns),
            )
            .await?;
            self.deps.delegation.complete(&run_id, &text).await?;
            Ok::<_, anyhow::Error>(text)
        }
        .await;
        match result {
            Ok(report) if child.cancel.is_cancelled() => {
                let _ = self
                    .deps
                    .delegation
                    .interrupt(&run_id, "delegated agent interrupted")
                    .await;
                MainDelegationResult {
                    run_id,
                    agent_id,
                    status: SubtaskStatus::Interrupted,
                    report,
                }
            }
            Ok(report) => MainDelegationResult::completed(run_id, agent_id, report),
            Err(error) if child.cancel.is_cancelled() => {
                let _ = self
                    .deps
                    .delegation
                    .interrupt(&run_id, "delegated agent interrupted")
                    .await;
                MainDelegationResult {
                    run_id,
                    agent_id,
                    status: SubtaskStatus::Interrupted,
                    report: error.to_string(),
                }
            }
            Err(error) => {
                let _ = self.deps.delegation.fail(&run_id, &error.to_string()).await;
                MainDelegationResult {
                    run_id,
                    agent_id,
                    status: SubtaskStatus::Error,
                    report: error.to_string(),
                }
            }
        }
    }

    async fn deliver_background_result(
        &self,
        originating_run_id: &str,
        goal: &str,
        result: &MainDelegationResult,
    ) {
        let block = delegation::format_delegation_block(&delegation::DelegationResult {
            id: result.run_id.clone(),
            goal: goal.to_string(),
            agent_type: result.agent_id.clone(),
            model: self.deps.model.clone().unwrap_or_default(),
            status: result.status.as_str().to_string(),
            summary: result.report.clone(),
            error: (result.status == SubtaskStatus::Error).then(|| result.report.clone()),
        });
        if let Err(error) = self
            .deps
            .store
            .enqueue_background_delegation_event(&self.deps.session_pk, originating_run_id, &block)
            .await
        {
            tracing::warn!(
                "native: failed to enqueue background main delegation {}: {error}",
                result.run_id,
            );
        }
    }
}

#[async_trait]
impl MainAgentSpawner for RunnerMainAgentSpawner {
    async fn available(&self) -> Vec<(String, String, String)> {
        self.deps
            .delegation
            .delegate_catalog(&self.deps.primary_agent.profile.id)
            .await
    }

    async fn run_one(
        &self,
        request: crate::delegation::MainDelegationRequest,
    ) -> MainDelegationResult {
        self.run_child(request).await
    }

    async fn run_many(
        &self,
        requests: Vec<crate::delegation::MainDelegationRequest>,
    ) -> Vec<MainDelegationResult> {
        futures::future::join_all(requests.into_iter().map(|request| self.run_child(request))).await
    }
}

impl RunnerSpawner {
    /// The `max_concurrent_runs` setting (default 3, floor 1).
    async fn concurrency(&self) -> usize {
        crate::settings::usize_setting(&self.deps.store, "max_concurrent_runs", 3).await
    }

    /// The parent session's remaining context headroom in tokens (usable
    /// window − active), read from its persisted context. 0 when unknown.
    async fn parent_headroom_tokens(&self) -> u64 {
        match self
            .deps
            .store
            .get_session_context(&self.deps.session_pk)
            .await
        {
            Ok(Some(saved)) => {
                let usable = saved["usable_window"].as_u64().unwrap_or(0);
                let active = saved["active_tokens"].as_u64().unwrap_or(0);
                usable.saturating_sub(active)
            }
            _ => 0,
        }
    }

    /// Run one delegated child to completion; failures become the result's
    /// status, never a panic or batch abort.
    async fn run_child(
        &self,
        source_tool_call_id: &str,
        index: usize,
        spec: SubtaskSpec,
        _cancel: CancellationToken,
    ) -> SubtaskResult {
        let result = |status, report| SubtaskResult {
            index,
            agent_type: spec.agent_type.clone(),
            status,
            report,
        };
        let child_run = match self
            .deps
            .delegation
            .queue_subagent(SubagentRunRequest {
                parent_run_id: self.parent_run_id.clone(),
                subagent_type: spec.agent_type.clone(),
                task: spec.prompt.clone(),
                context: None,
                background: false,
                dispatch: Some(crate::delegation::AgentDispatchLink {
                    source_tool_call_id: source_tool_call_id.to_string(),
                    dispatch_index: i64::try_from(index).expect("subtask index fits i64"),
                }),
            })
            .await
        {
            Ok(child) => child,
            Err(error) => return result(SubtaskStatus::Error, error.to_string()),
        };
        self.run_queued_child(index, spec, child_run.cancel.clone(), child_run)
            .await
    }

    /// Execute a child after its durable run has been admitted. The same path
    /// powers synchronous single/batch calls and detached background work.
    async fn run_queued_child(
        &self,
        index: usize,
        spec: SubtaskSpec,
        _cancel: CancellationToken,
        child_run: RunHandle,
    ) -> SubtaskResult {
        let run_id = child_run.run.run_id.clone();
        let result = |status, report| SubtaskResult {
            index,
            agent_type: spec.agent_type.clone(),
            status,
            report,
        };
        if let Err(error) = self.deps.delegation.mark_running(&run_id).await {
            return result(SubtaskStatus::Error, error.to_string());
        }
        let child_cancel = child_run.cancel.clone();
        let execution = async {
            self.run_subagent_loop(index, &spec, child_cancel, &run_id)
                .await
        }
        .await;
        match execution {
            SubtaskResult {
                status: SubtaskStatus::Completed,
                report,
                ..
            } => match self.deps.delegation.complete(&run_id, &report).await {
                Ok(()) => result(SubtaskStatus::Completed, report),
                Err(error) => result(SubtaskStatus::Error, error.to_string()),
            },
            SubtaskResult {
                status: SubtaskStatus::Interrupted,
                report,
                ..
            } => {
                let _ = self
                    .deps
                    .delegation
                    .interrupt(&run_id, "subagent interrupted")
                    .await;
                result(SubtaskStatus::Interrupted, report)
            }
            SubtaskResult {
                status: SubtaskStatus::Error,
                report,
                ..
            } => {
                let _ = self.deps.delegation.fail(&run_id, &report).await;
                result(SubtaskStatus::Error, report)
            }
        }
    }

    /// Run one bounded, memoryless `task` child after durable admission.
    async fn run_subagent_loop(
        &self,
        index: usize,
        spec: &SubtaskSpec,
        cancel: CancellationToken,
        run_id: &str,
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
        // Delegating children get the `task` tool re-armed.
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
                        None,
                        None,
                    )
                ),
            });
        }
        // Tool rows only (tagged with the sub-agent label), no memory access;
        // history is ephemeral. Sub-agents also get NO app-control facade — the
        // curated app surface is a top-level-session capability only (spec §9.1).
        // Reset it here so the `ctx.app == None for sub-agents` invariant holds
        // unconditionally at the facade layer, not merely because the SUBAGENT
        // name-blocklist (Task 8) hides the tools — defense in depth.
        let mut child_deps = match deps_for_subagent(&self.deps).await {
            Ok(deps) => deps,
            Err(error) => return result(SubtaskStatus::Error, error.to_string()),
        };
        child_deps.run_id = run_id.to_string();
        child_deps.agent = child.clone();
        let child_spawn: Option<Arc<dyn SubagentSpawner>> = if delegates {
            Some(Arc::new(RunnerSpawner {
                deps: child_deps.clone(),
                cancel: cancel.clone(),
                depth: child_depth,
                parent_run_id: run_id.to_string(),
            }))
        } else {
            None
        };
        let mut cm = ContextManager::ephemeral(
            &self.deps.session_pk,
            ContextConfig::with_meta(self.deps.meta.clone()),
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
            Err(error) => result(SubtaskStatus::Error, error.to_string()),
        }
    }
}

/// Dispatch an admitted main-delegate retry through the same profile-isolated
/// runner as a normal `delegate_agent` execution. The pre-existing session
/// harness supplies the current project/worktree/MCP tool registry; the child
/// snapshot supplies the effective target profile.
pub(crate) fn dispatch_retry_main_delegate(
    deps: RunnerDeps,
    child: RunHandle,
) -> anyhow::Result<()> {
    if child.run.agent_kind != crate::domain::AgentRunKind::MainDelegate {
        anyhow::bail!("only main-delegate retries can use the main-delegate executor");
    }
    let worker = RunnerMainAgentSpawner { deps };
    tokio::spawn(async move {
        let _ = worker.execute_child(child, None).await;
    });
    Ok(())
}

/// Dispatch an admitted subagent retry through the existing queued-child
/// executor. The caller supplies a freshly started session harness so the
/// retry inherits the current session configuration while retaining the
/// persisted subagent type and task.
pub(crate) fn dispatch_retry_subagent(deps: RunnerDeps, child: RunHandle) -> anyhow::Result<()> {
    if child.run.agent_kind != crate::domain::AgentRunKind::Subagent {
        anyhow::bail!("only subagent retries can use the subagent executor");
    }
    let spec = SubtaskSpec {
        agent_type: child.run.executing_agent_name_snapshot.clone(),
        prompt: child.run.task.clone(),
    };
    let cancel = child.cancel.clone();
    let spawner = RunnerSpawner {
        parent_run_id: child.run.parent_run_id.clone().unwrap_or_default(),
        deps,
        cancel: cancel.clone(),
        depth: 0,
    };
    tokio::spawn(async move {
        let _ = spawner.run_queued_child(0, spec, cancel, child).await;
    });
    Ok(())
}

#[async_trait]
impl SubagentSpawner for RunnerSpawner {
    async fn run_many(
        &self,
        source_tool_call_id: &str,
        specs: Vec<SubtaskSpec>,
    ) -> Vec<SubtaskResult> {
        let sem = Arc::new(tokio::sync::Semaphore::new(self.concurrency().await));
        let dispatches = specs.into_iter().enumerate().collect::<Vec<_>>();
        let futures = dispatches.into_iter().map(|(index, spec)| {
            let sem = sem.clone();
            let cancel = self.cancel.child_token();
            let source_tool_call_id = source_tool_call_id.to_string();
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
                self.run_child(&source_tool_call_id, index, spec, cancel)
                    .await
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

    async fn run_background(
        &self,
        source_tool_call_id: &str,
        spec: SubtaskSpec,
    ) -> BackgroundDispatch {
        // Background delegation is a top-level capability only; a nested
        // (delegated) spawner must not fan out detached workers.
        if self.depth != 0 {
            return BackgroundDispatch::Rejected {
                note: "background delegation is only available at the top level; \
                       run this task synchronously (background=false)."
                    .to_string(),
            };
        }
        let cap = self.concurrency().await; // shared max_concurrent_runs (default 3)
        let Some(reservation) = self.deps.background.try_reserve(cap, &self.deps.session_pk) else {
            // Hermes' choice (async_delegation.py:196-206): reject with a
            // fallback-to-sync note, never queue. Wording adapted only where
            // Ryuzi's mechanism differs from Hermes' (no config.yaml; the
            // setting is `max_concurrent_runs`, not
            // `delegation.max_concurrent_children`) — everything else is
            // byte-for-byte the same sentence structure and wording.
            return BackgroundDispatch::Rejected {
                note: format!(
                    "Async delegation capacity reached ({cap} running). Wait for one to \
                     finish (its result will re-enter the chat), or run this task \
                     synchronously (background=false). Raise max_concurrent_runs to allow \
                     more concurrent background subagents."
                ),
            };
        };
        let child_run = match self
            .deps
            .delegation
            .queue_subagent(SubagentRunRequest {
                parent_run_id: self.parent_run_id.clone(),
                subagent_type: spec.agent_type.clone(),
                task: spec.prompt.clone(),
                context: None,
                background: true,
                dispatch: Some(crate::delegation::AgentDispatchLink {
                    source_tool_call_id: source_tool_call_id.to_string(),
                    dispatch_index: 0,
                }),
            })
            .await
        {
            Ok(child) => child,
            Err(error) => {
                return BackgroundDispatch::Rejected {
                    note: format!("background delegation could not be queued: {error}"),
                };
            }
        };
        let id = child_run.run.run_id.clone();
        let deps = self.deps.clone();
        let child_cancel = child_run.cancel.clone();
        let this_spawner = RunnerSpawner {
            deps: deps.clone(),
            cancel: child_cancel.clone(),
            depth: 0,
            parent_run_id: child_run.run.run_id.clone(),
        };
        let (deleg_id, goal) = (id.clone(), spec.prompt.clone());
        let (parent_pk, root_run_id) = (deps.session_pk.clone(), self.deps.root_run_id.clone());
        // Read the parent's persisted headroom on the CALLER's task (a quick
        // DB read) before detaching — the spawned worker only needs the
        // resulting number, not a live store round-trip mid-flight.
        let headroom = self.parent_headroom_tokens().await;
        tokio::spawn(async move {
            // Holding the reservation for the task's whole lifetime keeps the
            // slot taken; its Drop (on completion, panic, or cancellation)
            // frees the slot and deregisters the cancel token.
            let _reservation = reservation;
            let reservation_cancel = _reservation.token();
            let child = this_spawner
                .run_queued_child(0, spec, child_cancel.clone(), child_run)
                .await;
            // A cancelled worker (its parent ended, or was interrupted via
            // `interrupt_for_session`) must not write a stale completion to
            // the rail — the session that would receive it may already be
            // gone, or a fresh one may have taken its session_pk.
            if reservation_cancel.is_cancelled() || child_cancel.is_cancelled() {
                let _ = deps
                    .delegation
                    .interrupt(&deleg_id, "background subagent interrupted")
                    .await;
                return;
            }
            let cap_chars = summary_budget::budget_cap_chars(headroom, 1);
            let spill_dir = crate::paths::chat_scratch_dir(&parent_pk).join("delegations");
            let budgeted =
                summary_budget::budget_summary(&child.report, cap_chars, &spill_dir, &deleg_id);
            let block = delegation::format_delegation_block(&delegation::DelegationResult {
                id: deleg_id.clone(),
                goal,
                agent_type: child.agent_type.clone(),
                model: deps.model.clone().unwrap_or_default(),
                status: child.status.as_str().to_string(),
                summary: budgeted.text,
                error: (child.status == SubtaskStatus::Error).then(|| child.report.clone()),
            });
            // Durable re-entry (survives a daemon restart), scoped to the
            // primary run that dispatched this child so the rail records a
            // delegation result instead of opening a synthetic user turn.
            let _ = deps
                .store
                .enqueue_background_delegation_event(&parent_pk, &root_run_id, &block)
                .await;
        });
        BackgroundDispatch::Dispatched { id }
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
    if t.name == LOAD_TOOLS_NAME {
        return handle_load_tools(deps, agent, t, display).await;
    }
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
    if insert_tool_row(deps, t, &input, tool.kind(), display.subagent()).await {
        if let Err(error) = deps
            .store
            .increment_agent_run_tool_count(&deps.run_id)
            .await
        {
            tracing::warn!(
                "native: increment_agent_run_tool_count({}) failed: {error}",
                deps.run_id
            );
        }
    }

    // Plugin hooks: a `tool.before` hook (script or extension) may deny the
    // call — see `hooks::fire_hook`'s combine contract.
    let hook = super::hooks::fire_hook(
        &deps.work_dir,
        deps.extension_events.as_ref(),
        super::hooks::HookEvent::ToolBefore,
        &json!({ "tool": t.name, "input": input }),
    )
    .await;
    crate::automation::dispatch_lifecycle_observation(
        deps.automation_events.clone(),
        crate::automation::TriggerKind::ToolBefore,
        deps.session_pk.clone(),
        json!({ "tool": t.name, "input": input }),
    );
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
        permission_rules: &agent.permission_rules,
        perm_mode,
        project_id: deps.project_id.as_deref(),
        store: &deps.store,
        overrides: &deps.perm_overrides,
        session_pk: &deps.session_pk,
        run_id: &deps.run_id,
        requesting_agent_id: &deps.primary_agent.profile.id,
        requesting_agent_name: &agent.name,
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
        run_id: deps.run_id.clone(),
        work_dir: deps.work_dir.clone(),
        attachments_dir: deps.attachments_dir.clone(),
        extra_skill_dirs: deps.extra_skill_dirs.clone(),
        store: deps.store.clone(),
        cancel: cancel.clone(),
        caps: OutputCaps::default(),
        spawn: spawn.clone(),
        main_agent_spawn: Some(Arc::new(RunnerMainAgentSpawner { deps: deps.clone() })),
        memory: deps.memory.clone(),
        snapshots: deps.snapshots.clone(),
        tool_call_id: t.id.clone(),
        interaction: Some(Arc::new(super::tools::Interaction {
            approvals: deps.approvals.clone(),
            events: deps.events.clone(),
            run_id: deps.run_id.clone(),
            requesting_agent_id: deps.primary_agent.profile.id.clone(),
            requesting_agent_name: agent.name.clone(),
            perm_mode: deps.perm_mode.clone(),
            project_id: deps.project_id.clone(),
        })),
        app: deps.app_control.clone(),
        write_origin: deps.write_origin,
        viewed_skills: Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new())),
    };
    // Keep a copy for the `tool.after` payload below — `execute` consumes
    // `input` by value.
    let hook_input = input.clone();
    let (tool_use_result, after_summary) = match tool.execute(&ctx, input).await {
        Ok(mut out) => {
            let extras = merge_display_duration(out.display.take(), elapsed_ms(started));
            finish_tool_row_with_display(deps, &t.id, &out.for_model, out.is_error, Some(extras))
                .await;
            let is_error = out.is_error;
            let summary = json!({
                "ok": !is_error,
                "output": truncate_for_context(&out.for_model, TOOL_AFTER_OUTPUT_BYTES),
            });
            let result = match out.model_blocks.take() {
                Some(mut blocks) => {
                    blocks.push(json!({ "type": "text", "text": out.for_model }));
                    json!({
                        "type": "tool_result",
                        "tool_use_id": t.id,
                        "content": blocks,
                        "is_error": is_error,
                    })
                }
                None => tool_result(&t.id, &out.for_model, is_error),
            };
            (result, summary)
        }
        Err(e) => {
            let msg = format!("{}: {e}", t.name);
            let extras = merge_display_duration(None, elapsed_ms(started));
            finish_tool_row_with_display(deps, &t.id, &msg, true, Some(extras)).await;
            let summary = json!({
                "ok": false,
                "error": truncate_for_context(&msg, TOOL_AFTER_OUTPUT_BYTES),
            });
            (tool_result(&t.id, &msg, true), summary)
        }
    };
    // Observational: never gates, result ignored. Fires for both Ok and Err
    // outcomes now that the `ToolOutput` (or its error) has resolved.
    let after_payload = json!({ "tool": t.name, "input": hook_input, "result": after_summary });
    let _ = super::hooks::fire_hook(
        &deps.work_dir,
        deps.extension_events.as_ref(),
        super::hooks::HookEvent::ToolAfter,
        &after_payload,
    )
    .await;
    crate::automation::dispatch_lifecycle_observation(
        deps.automation_events.clone(),
        crate::automation::TriggerKind::ToolAfter,
        deps.session_pk.clone(),
        after_payload,
    );
    tool_use_result
}

/// Handle the synthetic `load_tools` meta-call: activate the requested deferred
/// tools into `deps.activated_tools` so they are advertised from the next
/// provider turn. Validated against the tools this agent may actually load
/// (registry tools that are allowed, not hot, and not gated out). Skips the
/// permission gate and worktree snapshot — it mutates advertisement state only.
async fn handle_load_tools(
    deps: &RunnerDeps,
    agent: &Agent,
    t: &ToolAccum,
    display: &DisplayMode,
) -> Value {
    let input = t.parsed_input();
    insert_tool_row(deps, t, &input, "other", display.subagent()).await;

    let Some(activated) = deps.activated_tools.as_ref() else {
        let msg = "load_tools is not available in this session";
        finish_tool_row(deps, &t.id, msg, true).await;
        return tool_result(&t.id, msg, true);
    };

    let loadable: std::collections::BTreeSet<String> = deps
        .tools
        .names()
        .into_iter()
        .filter(|n| agent.tools.allows(n) && !is_hot(n))
        .collect();

    let requested: Vec<String> = input
        .get("names")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if requested.is_empty() {
        let msg = "No tool names given. Provide the exact tool names to load, taken from the load_tools description.";
        finish_tool_row(deps, &t.id, msg, true).await;
        return tool_result(&t.id, msg, true);
    }

    let (mut loaded, mut unknown) = (Vec::new(), Vec::new());
    for name in requested {
        if loadable.contains(&name) {
            loaded.push(name);
        } else {
            unknown.push(name);
        }
    }
    if !loaded.is_empty() {
        let mut set = activated.lock().await;
        for n in &loaded {
            set.insert(n.clone());
        }
    }

    let is_error = loaded.is_empty();
    let msg = if unknown.is_empty() {
        format!(
            "Loaded: {}. These tools are now available to call.",
            loaded.join(", ")
        )
    } else if loaded.is_empty() {
        format!(
            "No tools loaded. Unknown or unavailable: {}. Loadable tools: {}.",
            unknown.join(", "),
            loadable.into_iter().collect::<Vec<_>>().join(", ")
        )
    } else {
        format!(
            "Loaded: {}. Ignored (unknown/unavailable): {}.",
            loaded.join(", "),
            unknown.join(", ")
        )
    };
    finish_tool_row(deps, &t.id, &msg, is_error).await;
    tool_result(&t.id, &msg, is_error)
}

/// Insert the initial `tool_call` row (`{name, input}`, in_progress).
async fn insert_tool_row(
    deps: &RunnerDeps,
    t: &ToolAccum,
    input: &Value,
    kind: &str,
    subagent: Option<&str>,
) -> bool {
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
    .await
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
                speaker: None,
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
) -> bool {
    let msg = NewMessage {
        session_pk: deps.session_pk.clone(),
        role: role.to_string(),
        block_type: block_type.to_string(),
        payload: payload.clone(),
        tool_call_id: tool_call_id.clone(),
        status: status.clone(),
        tool_kind: tool_kind.clone(),
        speaker: None,
    };
    match deps.store.insert_run_message(&deps.run_id, msg).await {
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
                speaker: None,
            });
            true
        }
        Err(e) => {
            tracing::warn!("native[{NATIVE_ID}]: insert_message failed: {e}");
            false
        }
    }
}

async fn observe_route_selection(
    deps: &RunnerDeps,
    context: &RouteObservationContext,
    selection: &RouteSelection,
) {
    match deps
        .store
        .observe_session_route(&context.session_pk, selection)
        .await
    {
        Ok(Some(message)) => {
            let _ = deps.events.send(CoreEvent::Message {
                session_pk: message.session_pk,
                seq: message.seq,
                role: message.role,
                block_type: message.block_type,
                payload: message.payload,
                tool_call_id: message.tool_call_id,
                status: message.status,
                tool_kind: message.tool_kind,
                speaker: message.speaker,
            });
        }
        Ok(None) => {}
        Err(error) => tracing::warn!(
            "native[{NATIVE_ID}]: observe_session_route failed for {}: {error}",
            context.session_pk
        ),
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
        cache_creation_tokens: cm.last_cache_creation(),
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
        cache_creation_tokens: cm.last_cache_creation(),
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
            .cloned()
            .unwrap_or_else(|| crate::llm_router::model_meta::FALLBACK.clone())
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
    use crate::llm_router::model_effort::TurnEffortPolicy;
    use crate::llm_router::provenance::{
        LlmRequest, LlmRequestMetadata, RouteSelection, RouteSelectionReason, RoutedStream,
    };
    use async_trait::async_trait;
    use serde_json::Value;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    /// An `LlmStream` that replays scripted turns: the first `stream()` call
    /// returns turn 0's events, the next returns turn 1's, and so on.
    pub struct ScriptedLlm {
        turns: Mutex<std::collections::VecDeque<(RouteSelection, Vec<AnthropicEvent>)>>,
        pub metadata: Mutex<Vec<LlmRequestMetadata>>,
    }

    impl ScriptedLlm {
        pub fn new(turns: Vec<Vec<AnthropicEvent>>) -> Self {
            ScriptedLlm {
                turns: Mutex::new(
                    turns
                        .into_iter()
                        .map(|events| (test_route_selection(), events))
                        .collect(),
                ),
                metadata: Mutex::new(Vec::new()),
            }
        }

        pub fn with_selections(turns: Vec<(RouteSelection, Vec<AnthropicEvent>)>) -> Self {
            Self {
                turns: Mutex::new(turns.into_iter().collect()),
                metadata: Mutex::new(Vec::new()),
            }
        }
    }

    pub fn test_route_selection() -> RouteSelection {
        RouteSelection {
            requested_model: "test/model".into(),
            resolved_provider_id: "test".into(),
            resolved_family: "test".into(),
            resolved_model: "model".into(),
            resolved_model_display_name: "Test Model".into(),
            effective_effort: None,
            effective_effort_label: None,
            connection_id: "ignored".into(),
            connection_label: "Ignored".into(),
            reason: RouteSelectionReason::Initial,
        }
    }

    #[async_trait]
    impl LlmStream for ScriptedLlm {
        async fn stream(&self, request: LlmRequest) -> anyhow::Result<RoutedStream> {
            self.metadata.lock().unwrap().push(request.metadata);
            let (selection, events) = self
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
            Ok(RoutedStream {
                selection,
                events: rx,
            })
        }
    }

    /// Wraps [`ScriptedLlm`], recording every request body for assertions.
    pub struct RecordingLlm {
        inner: ScriptedLlm,
        pub bodies: Mutex<Vec<Value>>,
        pub policies: Mutex<Vec<Arc<TurnEffortPolicy>>>,
    }

    impl RecordingLlm {
        pub fn new(turns: Vec<Vec<AnthropicEvent>>) -> Self {
            RecordingLlm {
                inner: ScriptedLlm::new(turns),
                bodies: Mutex::new(Vec::new()),
                policies: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LlmStream for RecordingLlm {
        async fn stream(&self, request: LlmRequest) -> anyhow::Result<RoutedStream> {
            self.bodies.lock().unwrap().push(request.body.clone());
            self.policies
                .lock()
                .unwrap()
                .push(request.metadata.effort_policy.clone());
            self.inner.stream(request).await
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
    use async_trait::async_trait;
    use serial_test::serial;

    struct StaticMcpCaller;

    struct BlockingTool {
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        effects: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl crate::harness::native::tools::Tool for BlockingTool {
        fn name(&self) -> &str {
            "blocking"
        }

        fn description(&self) -> &str {
            "Blocks until the test releases it."
        }

        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }

        fn kind(&self) -> &'static str {
            "other"
        }

        fn permission(&self, _input: &Value) -> crate::harness::native::tools::PermissionSpec {
            crate::harness::native::tools::PermissionSpec::new("blocking", "block test")
        }

        async fn execute(
            &self,
            _ctx: &crate::harness::native::tools::ToolCtx,
            _input: Value,
        ) -> anyhow::Result<crate::harness::native::tools::ToolOutput> {
            self.started.notify_one();
            self.release.notified().await;
            self.effects
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(crate::harness::native::tools::ToolOutput::ok("released"))
        }
    }

    #[async_trait]
    impl crate::harness::native::mcp_client::McpCaller for StaticMcpCaller {
        async fn call(
            &self,
            _tool: &str,
            _arguments: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            Ok(serde_json::json!({"content": []}))
        }
    }

    #[tokio::test]
    async fn primary_agent_model_drives_turn_configuration() {
        use crate::agents::types::{
            AgentAvatar, AgentLoop, AgentModel, AgentPermissions, AgentProfile, AgentSnapshot,
            AgentTools,
        };
        use testutil::RecordingLlm;

        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(RecordingLlm::new(vec![]));
        let mut deps = deps_at(dir.path(), llm).await;
        deps.primary_agent = Arc::new(AgentSnapshot {
            profile: AgentProfile {
                schema_version: 1,
                id: "primary".into(),
                name: "Primary".into(),
                description: String::new(),
                avatar: AgentAvatar {
                    color: "blue".into(),
                },
                model: AgentModel::Concrete {
                    name: "anthropic/model-b".into(),
                    effort: Some("high".into()),
                },
                permissions: AgentPermissions {
                    mode: PermMode::Default,
                    rules: vec![],
                },
                skills: vec![],
                tools: AgentTools {
                    native: vec![],
                    plugins: vec![],
                    apps: vec![],
                },
                loop_settings: AgentLoop {
                    max_turns: 1,
                    max_tool_rounds: 1,
                },
            },
            executable: true,
            validation: vec![],
        });
        deps.model = Some("anthropic/model-b".into());
        add_anthropic_conn(&deps.store, &["model-b"]).await;
        deps.store
            .set_setting_raw(
                "models.meta.anthropic/model-b",
                r#"{"context_window":222222}"#,
            )
            .await
            .unwrap();

        let refreshed = refresh_turn_configuration(&deps, None).await;
        assert_eq!(refreshed.model.as_deref(), Some("anthropic/model-b"));
        assert_eq!(refreshed.meta.context_window, 222_222);
        assert_eq!(
            refreshed.turn_effort_policy.caller_override.as_deref(),
            Some("high")
        );
    }
    use crate::store::Store;

    fn route_selection(
        connection_id: &str,
        label: &str,
    ) -> crate::llm_router::provenance::RouteSelection {
        let mut selection = test_route_selection();
        selection.connection_id = connection_id.into();
        selection.connection_label = label.into();
        selection
    }

    fn final_turn(text: &str) -> Vec<crate::llm_router::client::AnthropicEvent> {
        vec![text_delta(text), message_delta("end_turn"), message_stop()]
    }

    fn tool_turn() -> Vec<crate::llm_router::client::AnthropicEvent> {
        vec![
            tool_use_start(0, "call-1", "bash"),
            input_json_delta(0, "{\"command\":\"echo route\"}"),
            message_delta("tool_use"),
            message_stop(),
        ]
    }

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
        deps_with_store(dir, llm, store).await
    }

    async fn deps_with_store(
        dir: &std::path::Path,
        llm: Arc<dyn LlmStream>,
        store: Arc<Store>,
    ) -> RunnerDeps {
        deps_with_store_and_registry(dir, llm, store).await.0
    }

    async fn deps_with_executable_profile_registry(
        dir: &std::path::Path,
        llm: Arc<dyn LlmStream>,
        store: Arc<Store>,
    ) -> (RunnerDeps, Arc<crate::agents::registry::AgentRegistry>) {
        add_anthropic_conn_with_efforts(&store, &["parent-model", "target-model"]).await;
        crate::agents::bootstrap::ensure_default_routes(&store)
            .await
            .unwrap();
        deps_with_store_and_registry(dir, llm, store).await
    }

    async fn deps_with_store_and_registry(
        dir: &std::path::Path,
        llm: Arc<dyn LlmStream>,
        store: Arc<Store>,
    ) -> (RunnerDeps, Arc<crate::agents::registry::AgentRegistry>) {
        let (events, _rx) = broadcast::channel(256);
        let agents = Arc::new(AgentRegistry::builtin());
        let agent = agents.default_agent();
        let knowledge = Arc::new(crate::agents::knowledge::AgentKnowledgeStore::new(
            dir.join(".agent-config"),
        ));
        let learning_queue = Arc::new(crate::agents::learning_queue::LearningQueue::new(
            store.clone(),
            knowledge,
        ));
        let persistence = crate::agents::bootstrap::AgentPersistence::temporary(store.clone())
            .await
            .unwrap();
        let primary_agent = persistence
            .registry
            .resolved_snapshot("ryuzi")
            .await
            .unwrap();
        let registry = persistence.registry.clone();
        let agent_knowledge = persistence.knowledge.clone();
        let delegation = crate::delegation::DelegationRuntime::new(
            store.clone(),
            registry.clone(),
            events.clone(),
        );
        let mut deps = RunnerDeps {
            session_pk: "s1".into(),
            primary_agent,
            run_id: "r1".into(),
            root_run_id: "r1".into(),
            delegation,
            isolated_target: false,
            main_agent_id: "ryuzi".into(),
            learning_queue,
            agent_knowledge,
            kind: SessionKind::Chat,
            work_dir: dir.to_path_buf(),
            attachments_dir: None,
            extra_skill_dirs: vec![],
            extension_events: None,
            // bypassPermissions so the scripted bash tool runs without a prompt.
            model: Some("test/model".into()),
            turn_effort_policy: Arc::new(TurnEffortPolicy {
                requested_model: "test/model".into(),
                caller_override: None,
                route_targets: Default::default(),
                configured: Default::default(),
                surfaces: Default::default(),
            }),
            meta: crate::llm_router::model_meta::FALLBACK,
            perm_mode: Arc::new(std::sync::Mutex::new(PermMode::BypassPermissions)),
            project_id: None,
            perm_overrides: Arc::new(std::sync::Mutex::new(Default::default())),
            store,
            events,
            approvals: Arc::new(ApprovalHub::new()),
            automation_events: None,
            llm,
            tools: Arc::new(ToolRegistry::builtin()),
            agent,
            agents,
            commands: Arc::new(CommandRegistry::builtin()),
            allowed_skills: None,
            memory: None,
            snapshots: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            steer: SteerBuffer::new(),
            background: super::super::background::BackgroundRegistry::new(),
            app_control: None,
            nudge: Arc::new(NudgeState::default()),
            review_tool_defs: None,
            activated_tools: None,
            write_origin: crate::domain::WriteOrigin::User,
            delegation_catalog: Vec::new(),
        };
        seed_owned_session_with_root(&mut deps, "test root").await;
        (deps, registry)
    }

    async fn seed_owned_session_with_root(deps: &mut RunnerDeps, task: &str) {
        use crate::domain::{AgentIdentitySnapshot, Session, SessionStatus};

        deps.store
            .insert_session(Session {
                session_pk: deps.session_pk.clone(),
                primary_agent_id: Some(deps.primary_agent.profile.id.clone()),
                primary_agent_snapshot: Some(AgentIdentitySnapshot {
                    id: deps.primary_agent.profile.id.clone(),
                    name: deps.primary_agent.profile.name.clone(),
                    avatar_color: deps.primary_agent.profile.avatar.color.clone(),
                }),
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: Some("test root".into()),
                status: SessionStatus::Idle,
                perm_mode: PermMode::BypassPermissions,
                started_by: None,
                created_at: None,
                last_active: None,
                resume_attempts: 0,
                branch_owned: false,
                kind: SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
        let root = deps
            .delegation
            .begin_primary(&deps.session_pk, deps.primary_agent.clone(), task)
            .await
            .unwrap();
        deps.run_id = root.run.run_id.clone();
        deps.root_run_id = root.run.run_id;
        deps.agent = crate::harness::native::primary_turn_config_with_tools(
            deps.primary_agent.clone(),
            deps.run_id.clone(),
            deps.root_run_id.clone(),
            &deps.tools.names(),
        )
        .unwrap()
        .agent_tools;
    }

    /// Feature C1a: a real tool call (the bash tool, actually executed —
    /// `deps_at` sets `BypassPermissions`) must fire the `tool.after` hook
    /// once it resolves, carrying the tool name, its input, and a compact
    /// ok/output summary. This is distinct from the `hooks::run` unit tests
    /// in `hooks.rs`: it proves the real `run_tool_call` call site actually
    /// dispatches the event, not just that the dispatcher's contract is
    /// correct in isolation.
    #[cfg(unix)]
    #[tokio::test]
    async fn tool_after_hook_fires_once_the_tool_call_resolves() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let hook_dir = dir.path().join(".ryuzi/hooks/tool.after");
        std::fs::create_dir_all(&hook_dir).unwrap();
        let capture = dir.path().join("captured.json");
        let script = hook_dir.join("capture.sh");
        std::fs::write(&script, format!("#!/bin/sh\ncat > {}\n", capture.display())).unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let selection = route_selection("a", "Primary");
        let llm = Arc::new(ScriptedLlm::with_selections(vec![
            (selection.clone(), tool_turn()),
            (selection, final_turn("done")),
        ]));
        let deps = deps_at(dir.path(), llm).await;
        run_turn(
            &deps,
            TurnPrompt::text("go", "go"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let captured: Value =
            serde_json::from_str(&std::fs::read_to_string(&capture).unwrap()).unwrap();
        assert_eq!(captured["tool"], "bash");
        assert_eq!(captured["input"]["command"], "echo route");
        assert_eq!(captured["result"]["ok"], true);
        assert!(captured["result"]["output"]
            .as_str()
            .unwrap()
            .contains("route"));
    }

    /// A fixed [`crate::plugins::extension::ExtensionEvents`] fake that
    /// denies exactly one `HookEvent` with a fixed reason and allows
    /// everything else — enough to prove `RunnerDeps::extension_events` is
    /// actually wired through the real `run_tool_call` fire site (Track D,
    /// DT5), not just that `hooks::fire_hook`'s combine contract is correct
    /// in isolation (that's covered by `hooks.rs`'s own unit tests).
    struct FixedExtensionEvents {
        deny_event: crate::harness::native::hooks::HookEvent,
        reason: &'static str,
    }

    struct BlockingAutomationSink {
        entered: tokio::sync::mpsc::UnboundedSender<()>,
        release: std::sync::Arc<tokio::sync::Semaphore>,
    }

    #[async_trait::async_trait]
    impl crate::automation::AutomationEventSink for BlockingAutomationSink {
        async fn observe_lifecycle(
            &self,
            _trigger: crate::automation::TriggerKind,
            _session_pk: String,
            _data: Value,
        ) {
            let _ = self.entered.send(());
            let _permit = self
                .release
                .acquire()
                .await
                .expect("test semaphore stays open");
        }
    }

    #[async_trait::async_trait]
    impl crate::plugins::extension::ExtensionEvents for FixedExtensionEvents {
        async fn dispatch(
            &self,
            event: crate::harness::native::hooks::HookEvent,
            _payload: &Value,
        ) -> crate::harness::native::hooks::HookResult {
            if event == self.deny_event {
                crate::harness::native::hooks::HookResult {
                    allowed: false,
                    message: Some(self.reason.to_string()),
                }
            } else {
                crate::harness::native::hooks::HookResult::allow()
            }
        }
    }

    #[tokio::test]
    async fn blocked_lifecycle_sink_does_not_change_native_tool_gate_result() {
        let dir = tempfile::tempdir().unwrap();
        let selection = route_selection("a", "Primary");
        let llm = Arc::new(ScriptedLlm::with_selections(vec![
            (selection.clone(), tool_turn()),
            (selection, final_turn("done")),
        ]));
        let mut deps = deps_at(dir.path(), llm).await;
        let (entered, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let release = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let sink = Arc::new(BlockingAutomationSink {
            entered,
            release: release.clone(),
        });
        deps.automation_events = Some(sink.clone());
        deps.extension_events = Some(Arc::new(FixedExtensionEvents {
            deny_event: crate::harness::native::hooks::HookEvent::ToolBefore,
            reason: "blocked by policy extension",
        }));

        run_turn(
            &deps,
            TurnPrompt::text("go", "go"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let msgs = deps.store.list_messages("s1").await.unwrap();
        let tool_call = msgs
            .iter()
            .find(|m| m.block_type == "tool_call")
            .expect("a tool_call row must exist");
        assert_eq!(tool_call.payload["output"], "blocked by policy extension");
        tokio::time::timeout(std::time::Duration::from_secs(1), entered_rx.recv())
            .await
            .expect("automation lifecycle sink must run")
            .expect("automation lifecycle channel must remain open");
        release.add_permits(1);
    }

    #[tokio::test]
    async fn tool_before_extension_deny_blocks_the_real_tool_call() {
        let dir = tempfile::tempdir().unwrap();
        let selection = route_selection("a", "Primary");
        // Two scripted turns, exactly like `tool_after_hook_fires_once_...`:
        // the tool call, then the follow-up response the agent loop makes
        // once the (denied) tool_result is appended to history.
        let llm = Arc::new(ScriptedLlm::with_selections(vec![
            (selection.clone(), tool_turn()),
            (selection, final_turn("done")),
        ]));
        let mut deps = deps_at(dir.path(), llm).await;
        deps.extension_events = Some(Arc::new(FixedExtensionEvents {
            deny_event: crate::harness::native::hooks::HookEvent::ToolBefore,
            reason: "blocked by policy extension",
        }));

        run_turn(
            &deps,
            TurnPrompt::text("go", "go"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let msgs = deps.store.list_messages("s1").await.unwrap();
        let tool_call = msgs
            .iter()
            .find(|m| m.block_type == "tool_call")
            .expect("a tool_call row must exist");
        assert_eq!(tool_call.payload["output"], "blocked by policy extension");
    }

    #[tokio::test]
    async fn route_notice_first_visible_request_establishes_silent_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(ScriptedLlm::with_selections(vec![(
            route_selection("a", "Primary"),
            final_turn("hello"),
        )]));
        let deps = deps_at(dir.path(), llm.clone()).await;
        run_turn(
            &deps,
            TurnPrompt::text("hi", "hi"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        assert!(deps
            .store
            .list_messages("s1")
            .await
            .unwrap()
            .iter()
            .all(|message| message.block_type != "notice"));
        assert_eq!(llm.metadata.lock().unwrap().len(), 1);
        assert!(llm.metadata.lock().unwrap()[0].observation.is_some());
    }

    #[tokio::test]
    async fn route_notice_changed_visible_request_persists_and_broadcasts_before_content() {
        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(ScriptedLlm::with_selections(vec![
            (route_selection("a", "Primary"), final_turn("one")),
            (route_selection("b", "Backup"), final_turn("two")),
        ]));
        let deps = deps_at(dir.path(), llm).await;
        let mut events = deps.events.subscribe();
        run_turn(
            &deps,
            TurnPrompt::text("one", "one"),
            CancellationToken::new(),
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

        let messages = deps.store.list_messages("s1").await.unwrap();
        let notice = messages.iter().find(|m| m.block_type == "notice").unwrap();
        let content = messages
            .iter()
            .find(|m| m.role == "assistant" && m.payload["text"] == "two")
            .unwrap();
        assert!(notice.seq < content.seq);
        let broadcasts: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert!(broadcasts.iter().any(|event| matches!(event,
            CoreEvent::Message { seq, block_type, payload, .. }
                if *seq == notice.seq && block_type == "notice" && payload == &notice.payload
        )));
    }

    #[tokio::test]
    async fn route_notice_unchanged_tool_loop_request_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let selection = route_selection("a", "Primary");
        let llm = Arc::new(ScriptedLlm::with_selections(vec![
            (selection.clone(), tool_turn()),
            (selection, final_turn("done")),
        ]));
        let deps = deps_at(dir.path(), llm).await;
        run_turn(
            &deps,
            TurnPrompt::text("go", "go"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            deps.store
                .list_messages("s1")
                .await
                .unwrap()
                .iter()
                .filter(|m| m.block_type == "notice")
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn route_notice_changed_tool_loop_account_emits_again() {
        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(ScriptedLlm::with_selections(vec![
            (route_selection("a", "Primary"), tool_turn()),
            (route_selection("b", "Backup"), final_turn("done")),
        ]));
        let deps = deps_at(dir.path(), llm).await;
        run_turn(
            &deps,
            TurnPrompt::text("go", "go"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let messages = deps.store.list_messages("s1").await.unwrap();
        assert_eq!(
            messages.iter().filter(|m| m.block_type == "notice").count(),
            1
        );
        assert!(messages
            .iter()
            .any(|m| m.payload["text"] == "Account switched to Backup"));
    }

    #[tokio::test]
    async fn route_notice_subagent_title_and_compaction_have_no_observation_context() {
        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(ScriptedLlm::new(vec![
            final_turn("subagent"),
            final_turn("title"),
            final_turn("summary"),
        ]));
        let llm_dyn: Arc<dyn LlmStream> = llm.clone();
        let deps = deps_at(dir.path(), llm_dyn.clone()).await;
        let cfg = ContextConfig::load(&deps.store, deps.meta.clone()).await;
        let mut cm = ContextManager::load(deps.store.clone(), "subagent", cfg)
            .await
            .unwrap();
        cm.append_user(json!([{"type": "text", "text": "delegated"}]))
            .await
            .unwrap();
        let budget = IterationBudget::new(SUBAGENT_MAX_ITERS);
        drive(
            &deps,
            &deps.agent,
            &mut cm,
            &CancellationToken::new(),
            None,
            DisplayMode::ToolsOnly {
                label: "test".into(),
            },
            &budget,
        )
        .await
        .unwrap();
        for purpose in ["title", "compaction"] {
            super::super::llm::collect_text(
                &llm_dyn,
                json!({"purpose": purpose}),
                deps.turn_effort_policy.clone(),
            )
            .await
            .unwrap();
        }
        assert!(llm
            .metadata
            .lock()
            .unwrap()
            .iter()
            .all(|metadata| metadata.observation.is_none()));
    }

    /// Step 1 of Task 7's TDD brief: two end_turn turns with
    /// `memory.nudge_interval=2` must enqueue exactly one durable learning
    /// queue row for the main agent, carrying a cache-parity payload.
    #[tokio::test]
    async fn finalizer_enqueues_a_learning_row_every_nudge_interval() {
        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(ScriptedLlm::new(vec![final_turn("one"), final_turn("two")]));
        let deps = deps_at(dir.path(), llm).await;
        deps.store
            .set_setting(
                crate::domain::WriteOrigin::User,
                "memory.nudge_interval",
                "2",
            )
            .await
            .unwrap();
        run_turn(
            &deps,
            TurnPrompt::text("one", "one"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(
            deps.learning_queue
                .claim_next("ryuzi", "peek")
                .await
                .unwrap()
                .is_none(),
            "below the nudge_interval threshold — no learning row yet"
        );

        run_turn(
            &deps,
            TurnPrompt::text("two", "two"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let ev = deps
            .learning_queue
            .claim_next("ryuzi", "learner")
            .await
            .unwrap()
            .expect("threshold crossed on turn 2 — a learning row must be enqueued");
        let crate::agents::learning_queue::LearningEventPayload::Review(review) = ev.payload else {
            panic!("nudge producer must enqueue a review payload");
        };
        let payload: LearningPayload = serde_json::from_str(&review.body)
            .expect("learning payload must round-trip through JSON");
        assert_eq!(payload.review_kind, "memory");
        assert_eq!(payload.parent_session_pk, "s1");
        assert!(!payload.system.is_empty(), "system prefix must be captured");
        assert!(!payload.tool_defs.is_empty(), "tool_defs must be captured");
        assert!(!payload.messages.is_empty(), "messages must be captured");

        // Firing resets the counter: no second row is left queued.
        assert!(deps
            .learning_queue
            .claim_next("ryuzi", "learner2")
            .await
            .unwrap()
            .is_none());
    }

    /// No-recursion guard (global constraint): a sub-agent's own `drive()`
    /// loop shares the parent's `NudgeState` `Arc`, but its `DisplayMode` is
    /// `ToolsOnly`, not `Full` — `display.text()` is false, so the end_turn
    /// seam never calls `maybe_enqueue_review`. `nudge_interval=1` means the
    /// threshold is already crossable on the very first end_turn; if the
    /// top-level-only guard were missing this would immediately enqueue.
    /// (Task 9's review fork gets its own fresh `NudgeState` AND drives with
    /// a non-`Full` `DisplayMode` too, so the same mechanism will exclude it.)
    #[tokio::test]
    async fn subagent_end_turn_never_nudges_even_past_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(ScriptedLlm::new(vec![final_turn("subagent reply")]));
        let deps = deps_at(dir.path(), llm).await;
        deps.store
            .set_setting(
                crate::domain::WriteOrigin::User,
                "memory.nudge_interval",
                "1",
            )
            .await
            .unwrap();
        let cfg = ContextConfig::load(&deps.store, deps.meta.clone()).await;
        let mut cm = ContextManager::load(deps.store.clone(), "subagent", cfg)
            .await
            .unwrap();
        cm.append_user(json!([{"type": "text", "text": "delegated"}]))
            .await
            .unwrap();
        let budget = IterationBudget::new(SUBAGENT_MAX_ITERS);
        drive(
            &deps,
            &deps.agent,
            &mut cm,
            &CancellationToken::new(),
            None,
            DisplayMode::ToolsOnly {
                label: "test".into(),
            },
            &budget,
        )
        .await
        .unwrap();

        assert_eq!(
            deps.nudge
                .user_turns
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "a sub-agent's own end_turn must never touch the shared nudge counter"
        );
        assert!(
            deps.store
                .claim_deliverable_background_event("peek")
                .await
                .unwrap()
                .is_none(),
            "sub-agents must never enqueue a learning row (no recursion)"
        );
    }

    /// The cache-parity payload must survive a JSON round trip byte-for-byte
    /// (the review fork — Task 9 — decodes exactly this shape off the rail).
    #[test]
    fn learning_payload_round_trips_through_json() {
        let payload = LearningPayload {
            review_kind: "combined".into(),
            parent_session_pk: "parent-1".into(),
            model: "anthropic/model-a".into(),
            supports_prompt_cache: true,
            system: "you are ryuzi".into(),
            tool_defs: vec![json!({"name": "bash"})],
            messages: vec![json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})],
        };
        let encoded = serde_json::to_string(&payload).unwrap();
        let decoded: LearningPayload = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.review_kind, payload.review_kind);
        assert_eq!(decoded.parent_session_pk, payload.parent_session_pk);
        assert_eq!(decoded.model, payload.model);
        assert_eq!(decoded.supports_prompt_cache, payload.supports_prompt_cache);
        assert_eq!(decoded.system, payload.system);
        assert_eq!(decoded.tool_defs, payload.tool_defs);
        assert_eq!(decoded.messages, payload.messages);
    }

    /// The load-bearing Task 9 invariant: the review fork's FIRST request
    /// reproduces the captured payload's `system` (re-wrapped exactly like
    /// the parent's own request), `tools`, and `messages` prefix byte-for-
    /// byte — the whole point of prompt-cache parity — and a non-whitelisted
    /// tool call (`bash`, still present in the advertised `tools` array for
    /// cache parity) is refused at DISPATCH, not merely hidden from the
    /// model.
    #[tokio::test]
    async fn review_fork_replays_byte_identical_prefix_and_denies_a_non_whitelisted_tool() {
        let meta = crate::llm_router::model_meta::ModelMeta {
            supports_prompt_cache: true,
            ..crate::llm_router::model_meta::FALLBACK
        };
        // Build the captured prefix through the REAL ContextManager (the same
        // mechanism `maybe_enqueue_review` uses) so `payload.messages` carries
        // exactly the `cache_control` marker a live parent turn would have
        // produced — no hand-authored JSON standing in for the real shape.
        let mut seed_cm =
            ContextManager::ephemeral("parent-1", ContextConfig::with_meta(meta.clone()));
        seed_cm
            .append_user(json!([{"type": "text", "text": "hi"}]))
            .await
            .unwrap();
        seed_cm
            .append_assistant(json!([{"type": "text", "text": "hello"}]))
            .await
            .unwrap();
        let captured_messages = seed_cm.messages_for_request();

        let payload = LearningPayload {
            review_kind: "memory".into(),
            parent_session_pk: "parent-1".into(),
            model: "test/model".into(),
            supports_prompt_cache: true,
            system: "You are ryuzi, the parent agent.".into(),
            tool_defs: vec![
                json!({"name": "bash", "description": "run a shell command", "input_schema": {"type": "object"}}),
                json!({"name": "memory", "description": "persistent memory", "input_schema": {"type": "object"}}),
            ],
            messages: captured_messages,
        };

        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(RecordingLlm::new(vec![
            vec![
                tool_use_start(0, "call-1", "bash"),
                input_json_delta(0, "{\"command\":\"echo hi\"}"),
                message_delta("tool_use"),
                message_stop(),
            ],
            final_turn("Reviewed, nothing to add."),
        ]));
        let mut deps = deps_at(dir.path(), llm.clone()).await;
        deps.meta = meta.clone();
        deps.model = Some(payload.model.clone());
        deps.review_tool_defs = Some(payload.tool_defs.clone());
        deps.write_origin = crate::domain::WriteOrigin::BackgroundReview;

        let agent = review_agent(payload.system.clone());
        let cfg = ContextConfig::with_meta(deps.meta.clone());
        let mut cm = ContextManager::seed_projected("review-1", cfg, payload.messages.clone());
        cm.append_user_text(&review_prompt_text(&payload.review_kind))
            .await
            .unwrap();

        let final_text = drive_review(&deps, &agent, &mut cm, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(final_text, "Reviewed, nothing to add.");

        let bodies = llm.bodies.lock().unwrap();
        assert_eq!(
            bodies.len(),
            2,
            "a dispatch-time refusal must not skip the scripted second (final) turn"
        );

        let first = &bodies[0];
        assert_eq!(
            first["system"],
            json!([{ "type": "text", "text": payload.system, "cache_control": {"type": "ephemeral"} }]),
            "system must be re-wrapped EXACTLY like the parent's own request \
             (the two-branch formula in `drive()`, keyed on `supports_prompt_cache`)"
        );
        assert_eq!(
            first["tools"],
            json!(payload.tool_defs),
            "tools must be byte-identical to the captured payload, including `bash`"
        );
        let msgs = first["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), payload.messages.len() + 1);
        assert_eq!(
            &msgs[..payload.messages.len()],
            payload.messages.as_slice(),
            "the captured prefix must be byte-identical, unmodified by seeding or appending"
        );
        let review_turn = &msgs[payload.messages.len()];
        assert_eq!(review_turn["role"], "user");
        let review_text = review_turn["content"][0]["text"].as_str().unwrap();
        assert!(review_text.starts_with(MEMORY_REVIEW_PROMPT));
        assert!(review_text.contains(REVIEW_TOOL_RESTRICTION_NOTE));

        // The SECOND request's trailing tool_result proves `bash` was refused
        // at dispatch (the exact `run_tool_call` whitelist-deny wording), not
        // silently dropped, denied by the (bypassed) permission gate, or
        // actually executed.
        let second = &bodies[1];
        let msgs2 = second["messages"].as_array().unwrap();
        let denial = msgs2.last().unwrap();
        assert_eq!(denial["role"], "user");
        let result_block = &denial["content"][0];
        assert_eq!(result_block["type"], "tool_result");
        assert_eq!(result_block["is_error"], true);
        assert!(result_block["content"]
            .as_str()
            .unwrap()
            .contains("not permitted"));
    }

    /// `seed_digest` (the non-cache-parity fallback, used when the resolved
    /// review model differs from the payload's captured model) keeps only
    /// the trailing `tail` messages.
    #[test]
    fn seed_digest_keeps_only_the_last_tail_messages() {
        let msgs: Vec<Value> = (0..5)
            .map(|i| {
                json!({
                    "role": if i % 2 == 0 { "user" } else { "assistant" },
                    "content": [{"type": "text", "text": format!("m{i}")}],
                })
            })
            .collect();
        let cfg = ContextConfig::with_meta(crate::llm_router::model_meta::FALLBACK);
        let cm = ContextManager::seed_digest("review-1", cfg, msgs, 3);
        let seeded = cm.messages_for_request();
        assert_eq!(seeded.len(), 3);
        assert_eq!(seeded[0]["content"][0]["text"], "m2");
        assert_eq!(seeded[2]["content"][0]["text"], "m4");
    }

    /// Guard for `RYUZI_TEST_CONFIG_ROOT`, mirroring
    /// `tools::skill_manage::tests::ConfigRootGuard` — redirects
    /// `skills_install::skills_root()` into a tempdir so this module's
    /// `skill_manage`-exercising test never touches the real
    /// `~/.config/ryuzi/skills`. Process-global env — every test using it
    /// must be `#[serial]`.
    struct ReviewConfigRootGuard {
        _dir: tempfile::TempDir,
    }
    impl ReviewConfigRootGuard {
        fn new() -> (Self, std::path::PathBuf) {
            let dir = tempfile::tempdir().unwrap();
            std::env::set_var("RYUZI_TEST_CONFIG_ROOT", dir.path());
            let root = dir.path().join("skills");
            (ReviewConfigRootGuard { _dir: dir }, root)
        }
    }
    impl Drop for ReviewConfigRootGuard {
        fn drop(&mut self) {
            std::env::remove_var("RYUZI_TEST_CONFIG_ROOT");
        }
    }

    /// The load-bearing Task 6/9 handoff: a whitelisted `skill_manage create`
    /// call inside the review fork actually writes to disk (proving the
    /// fork's own perm_mode/whitelist let it through) AND is stamped
    /// `created_by="agent"` — which `skill_manage`'s `execute` only does when
    /// `ctx.write_origin.is_autonomous()`. If `run_tool_call` still hardcoded
    /// `WriteOrigin::User` (the pre-Task-9 state), this stamp would never
    /// appear — so this is a real regression test for the write_origin wire,
    /// not just a smoke test that the call didn't error.
    #[tokio::test]
    #[serial]
    async fn review_fork_writes_via_whitelisted_tools_carrying_background_review_write_origin() {
        let (_guard, skills_root) = ReviewConfigRootGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let create_args = json!({
            "action": "create",
            "name": "deploy",
            "description": "How to deploy",
            "body": "Run make deploy.",
        });
        let llm = Arc::new(RecordingLlm::new(vec![
            vec![
                tool_use_start(0, "call-1", "skill_manage"),
                input_json_delta(0, &create_args.to_string()),
                message_delta("tool_use"),
                message_stop(),
            ],
            final_turn("Created a new skill."),
        ]));
        let mut deps = deps_at(dir.path(), llm).await;
        deps.write_origin = crate::domain::WriteOrigin::BackgroundReview;

        let agent = review_agent("You are ryuzi, reviewing.".into());
        let cfg = ContextConfig::with_meta(deps.meta.clone());
        let mut cm = ContextManager::seed_projected(
            "review-1",
            cfg,
            vec![json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})],
        );
        cm.append_user_text(&review_prompt_text("skill"))
            .await
            .unwrap();

        let final_text = drive_review(&deps, &agent, &mut cm, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(final_text, "Created a new skill.");

        let md = std::fs::read_to_string(skills_root.join("deploy/SKILL.md"))
            .expect("skill_manage create must have written SKILL.md");
        assert!(md.contains("Run make deploy."));

        let usage = deps
            .store
            .get_skill_usage("deploy")
            .await
            .unwrap()
            .expect("skill_manage create must record skill_usage");
        assert_eq!(
            usage.created_by.as_deref(),
            Some("agent"),
            "the fork's ToolCtx must carry an autonomous write_origin \
             (BackgroundReview), not the hardcoded User of the pre-Task-9 runner"
        );
    }

    #[tokio::test]
    async fn route_notice_reload_reads_persisted_system_notice() {
        let dir = tempfile::tempdir().unwrap();
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db.path()).await.unwrap());
        let llm = Arc::new(ScriptedLlm::with_selections(vec![
            (route_selection("a", "Primary"), final_turn("one")),
            (route_selection("b", "Backup"), final_turn("two")),
        ]));
        let deps = deps_with_store(dir.path(), llm, store).await;
        run_turn(
            &deps,
            TurnPrompt::text("one", "one"),
            CancellationToken::new(),
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
        drop(deps);
        let reopened = Store::open(db.path()).await.unwrap();
        let reloaded = reopened.list_messages("s1").await.unwrap();
        assert!(reloaded.iter().any(|m| {
            m.role == "system"
                && m.block_type == "notice"
                && m.payload["text"] == "Account switched to Backup"
        }));
    }

    /// Seed a project (pinned to `model`) plus a TITLED session "s1" so the
    /// per-turn snapshot has rows to read while title generation stays off
    /// (an untitled session row would consume an extra scripted LLM turn).
    async fn seed_pinned_project(store: &Store, model: Option<&str>) {
        use crate::domain::Project;
        store
            .insert_project(Project {
                project_id: "p".into(),
                name: "p".into(),
                workdir: "/w".into(),
                source: None,
                model: model.map(str::to_string),
                effort: None,
                perm_mode: PermMode::BypassPermissions,
                created_at: Some(0),
                is_git: false,
            })
            .await
            .unwrap();
        store.set_session_project("s1", "p").await.unwrap();
        store.set_session_title("s1", "titled").await.unwrap();
    }

    async fn add_anthropic_conn_with_efforts(store: &Store, models: &[&str]) {
        use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};
        use crate::llm_router::model_effort::{DiscoveredModelMeta, ReasoningEffortOption};

        let effort = |value: &str| ReasoningEffortOption {
            value: value.into(),
            label: value.into(),
            description: None,
        };
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
                    models_override: Some(models.iter().map(|model| (*model).into()).collect()),
                    model_meta_overrides: Some(
                        models
                            .iter()
                            .map(|model| {
                                (
                                    (*model).into(),
                                    DiscoveredModelMeta {
                                        effort_options: Some(vec![effort("low"), effort("high")]),
                                        default_effort_advertised: true,
                                        default_effort: Some("low".into()),
                                        ..Default::default()
                                    },
                                )
                            })
                            .collect(),
                    ),
                    ..Default::default()
                },
                created_at: 1,
                updated_at: 1,
            },
        )
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
    async fn durable_primary_model_wins_over_a_project_pin() {
        use testutil::RecordingLlm;

        let dir = tempfile::tempdir().unwrap();
        let turn = vec![text_delta("ok"), message_delta("end_turn"), message_stop()];
        let llm = Arc::new(RecordingLlm::new(vec![turn]));
        let mut deps = deps_at(dir.path(), llm.clone()).await;
        deps.model = Some("anthropic/model-b".into());
        add_anthropic_conn(&deps.store, &["model-a", "model-b"]).await;
        seed_pinned_project(&deps.store, Some("anthropic/model-a")).await;

        run_turn(
            &deps,
            TurnPrompt::text("one", "one"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(llm.bodies.lock().unwrap()[0]["model"], "anthropic/model-b");
    }

    #[tokio::test]
    async fn scheduler_model_override_wins_over_project_and_primary_models() {
        use testutil::RecordingLlm;

        let dir = tempfile::tempdir().unwrap();
        let turn = vec![text_delta("ok"), message_delta("end_turn"), message_stop()];
        let llm = Arc::new(RecordingLlm::new(vec![turn]));
        let mut deps = deps_at(dir.path(), llm.clone()).await;
        deps.model = Some("anthropic/model-b".into());
        add_anthropic_conn(&deps.store, &["model-a", "model-b", "model-c"]).await;
        seed_pinned_project(&deps.store, Some("anthropic/model-a")).await;
        deps.store
            .with_conn(|connection| {
                connection.execute(
                    "UPDATE sessions SET started_by = 'scheduler' WHERE session_pk = 's1'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        deps.store
            .update_session_runtime_settings("s1", Some("anthropic/model-c".into()), None)
            .await
            .unwrap();

        run_turn(
            &deps,
            TurnPrompt::text("one", "one"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(llm.bodies.lock().unwrap()[0]["model"], "anthropic/model-c");
    }

    #[tokio::test]
    async fn command_subtask_uses_subagent_turn_budget() {
        use testutil::ScriptedLlm;

        let dir = tempfile::tempdir().unwrap();
        let deps = deps_at(dir.path(), Arc::new(ScriptedLlm::new(vec![]))).await;
        deps.store
            .set_setting(
                crate::domain::WriteOrigin::User,
                "agent.max_provider_turns",
                "7",
            )
            .await
            .unwrap();
        let options = turn_options(&super::super::commands::ResolvedCommand {
            prompt: "Ship now".into(),
            agent: None,
            model: None,
            subtask: true,
        });

        assert_eq!(
            max_provider_turns(&deps, &options).await,
            SUBAGENT_MAX_ITERS
        );
        assert_eq!(
            max_provider_turns(&deps, &TurnOptions::default()).await,
            7,
            "plain turns keep the configured normal budget"
        );
    }

    /// `TurnPrompt.force_subtask` is the caller seam automation Hook agent
    /// runs use to reach the exact same subagent budget a `subtask: true`
    /// slash command reaches — proven directly against `run_turn`'s options
    /// resolution rather than only the pure `turn_options` helper above.
    #[tokio::test]
    async fn turn_prompt_force_subtask_overrides_plain_text_turn_options() {
        use testutil::ScriptedLlm;

        let dir = tempfile::tempdir().unwrap();
        let deps = deps_at(dir.path(), Arc::new(ScriptedLlm::new(vec![]))).await;
        deps.store
            .set_setting(
                crate::domain::WriteOrigin::User,
                "agent.max_provider_turns",
                "7",
            )
            .await
            .unwrap();

        // Plain (non-command) text with no override keeps the configured
        // normal budget.
        let plain = TurnPrompt::text("hello", "hello");
        assert_eq!(plain.force_subtask, None);
        assert_eq!(max_provider_turns(&deps, &TurnOptions::default()).await, 7);

        // A hook-run TurnPrompt forcing subtask=true reaches the same
        // subagent budget as a `subtask: true` command, even though its
        // text is plain (not a `/command`).
        let mut hook_prompt = TurnPrompt::text("Review $EVENT", "Review $EVENT");
        hook_prompt.force_subtask = Some(true);
        let mut options = TurnOptions::default();
        if let Some(force_subtask) = hook_prompt.force_subtask {
            options.subtask = force_subtask;
        }
        assert_eq!(
            max_provider_turns(&deps, &options).await,
            SUBAGENT_MAX_ITERS
        );
    }

    #[tokio::test]
    #[serial]
    async fn missing_project_command_root_falls_back_to_active_worktree() {
        use testutil::RecordingLlm;

        let _guard = StateDirGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let canonical_workdir = dir.path().join("canonical-project");
        std::fs::create_dir_all(&canonical_workdir).unwrap();
        std::fs::create_dir_all(dir.path().join(".ryuzi/commands")).unwrap();
        std::fs::write(
            dir.path().join(".ryuzi/commands/fallback.md"),
            "Active-worktree fallback $ARGUMENTS",
        )
        .unwrap();
        let llm = Arc::new(RecordingLlm::new(vec![final_turn("done")]));
        let mut deps = deps_at(dir.path(), llm.clone()).await;
        deps.project_id = Some("deleted-project".into());
        deps.store
            .insert_project(crate::domain::Project {
                project_id: "deleted-project".into(),
                name: "deleted-project".into(),
                workdir: canonical_workdir.display().to_string(),
                source: None,
                model: None,
                effort: None,
                perm_mode: PermMode::BypassPermissions,
                created_at: None,
                is_git: false,
            })
            .await
            .unwrap();
        deps.store
            .with_conn(|conn| {
                conn.execute(
                    "DELETE FROM projects WHERE project_id='deleted-project'",
                    [],
                )
                .map(|_| ())
            })
            .await
            .unwrap();

        run_turn(
            &deps,
            TurnPrompt::text("/fallback command", "/fallback command"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let body = llm.bodies.lock().unwrap().pop().unwrap();
        assert!(
            body.to_string()
                .contains("Active-worktree fallback command"),
            "a missing project row must resolve slash commands from the active worktree"
        );
    }

    #[tokio::test]
    async fn command_model_overrides_the_project_model_for_one_turn() {
        use crate::domain::{PermMode, Project};
        use testutil::RecordingLlm;

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ryuzi/commands")).unwrap();
        std::fs::write(
            dir.path().join(".ryuzi/commands/ship.md"),
            "---\nmodel: anthropic/model-b\nsubtask: true\n---\nShip $ARGUMENTS",
        )
        .unwrap();
        let llm = Arc::new(RecordingLlm::new(vec![final_turn("done")]));
        let mut deps = deps_at(dir.path(), llm.clone()).await;
        deps.project_id = Some("p".into());
        deps.commands = Arc::new(CommandRegistry::load(dir.path()));
        add_anthropic_conn(&deps.store, &["model-a", "model-b"]).await;
        deps.store
            .insert_project(Project {
                project_id: "p".into(),
                name: "project".into(),
                workdir: dir.path().display().to_string(),
                source: None,
                model: Some("anthropic/model-a".into()),
                effort: None,
                perm_mode: PermMode::Default,
                created_at: None,
                is_git: false,
            })
            .await
            .unwrap();

        run_turn(
            &deps,
            TurnPrompt::text("/ship now", "/ship now"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let body = llm.bodies.lock().unwrap().pop().unwrap();
        assert_eq!(body["model"], "anthropic/model-b");
        // The command's `subtask: true` frontmatter controls only the turn's
        // runtime iteration budget (see `turn_options`/`max_provider_turns`);
        // it must never appear as a message/system-prompt field. The
        // available `task` tool's own description legitimately contains the
        // substring "subtasks", so assert on the user message content
        // specifically rather than the whole serialized body.
        assert_eq!(body["messages"][0]["content"][0]["text"], "Ship now");
    }
    #[tokio::test]
    async fn refresh_turn_configuration_reloads_model_effort_preferences_defaults_and_meta() {
        use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};
        use crate::llm_router::model_effort::{
            DiscoveredModelMeta, ModelPreferenceKey, ReasoningEffortOption,
        };
        use testutil::RecordingLlm;

        let dir = tempfile::tempdir().unwrap();
        let turn = || vec![text_delta("ok"), message_delta("end_turn"), message_stop()];
        let llm = Arc::new(RecordingLlm::new(vec![turn(), turn()]));
        let mut deps = deps_at(dir.path(), llm.clone()).await;
        deps.model = Some("anthropic/model-a".into());
        deps.project_id = Some("p".into());
        let option = |value: &str| ReasoningEffortOption {
            value: value.into(),
            label: value.into(),
            description: None,
        };
        connections::add_connection(
            &deps.store,
            ConnectionRow {
                id: "claude".into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: "claude".into(),
                priority: 0,
                enabled: true,
                data: ConnectionData {
                    api_key: Some("sk-test".into()),
                    models_override: Some(vec!["model-a".into(), "model-b".into()]),
                    model_meta_overrides: Some(std::collections::HashMap::from([
                        (
                            "model-a".into(),
                            DiscoveredModelMeta {
                                effort_options: Some(vec![option("low"), option("high")]),
                                default_effort_advertised: true,
                                default_effort: Some("low".into()),
                                ..Default::default()
                            },
                        ),
                        (
                            "model-b".into(),
                            DiscoveredModelMeta {
                                effort_options: Some(vec![option("ultra")]),
                                ..Default::default()
                            },
                        ),
                    ])),
                    ..Default::default()
                },
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
        seed_pinned_project(&deps.store, Some("anthropic/model-a")).await;
        deps.store
            .update_project_runtime("p", Some("anthropic/model-a".into()), Some("high".into()))
            .await
            .unwrap();
        let key_a = ModelPreferenceKey {
            family: "anthropic".into(),
            model: "model-a".into(),
        };
        deps.store
            .set_model_effort_preference(&key_a, "low")
            .await
            .unwrap();
        deps.store
            .set_setting_raw(
                "models.meta.anthropic/model-a",
                r#"{"context_window":111111}"#,
            )
            .await
            .unwrap();

        run_turn(
            &deps,
            TurnPrompt::text("one", "one"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        deps.store
            .update_project_runtime("p", Some("anthropic/model-b".into()), None)
            .await
            .unwrap();
        deps.store
            .clear_model_effort_preference(&key_a)
            .await
            .unwrap();
        // The durable primary model wins over the project pin; update it before
        // the next turn so this test continues to exercise model-specific
        // effort and metadata refresh without relying on project precedence.
        deps.model = Some("anthropic/model-b".into());
        let key_b = ModelPreferenceKey {
            family: "anthropic".into(),
            model: "model-b".into(),
        };
        deps.store
            .set_model_effort_preference(&key_b, "ultra")
            .await
            .unwrap();
        deps.store
            .set_setting_raw(
                "models.meta.anthropic/model-b",
                r#"{"context_window":222222}"#,
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
            assert_eq!(bodies[0]["model"], "anthropic/model-a");
            assert_eq!(bodies[1]["model"], "anthropic/model-b");
        }
        {
            let policies = llm.policies.lock().unwrap();
            assert_eq!(policies[0].requested_model, "anthropic/model-a");
            assert_eq!(policies[0].caller_override.as_deref(), Some("high"));
            assert_eq!(
                policies[0].configured.get(&key_a).map(String::as_str),
                Some("low")
            );
            assert_eq!(policies[1].requested_model, "anthropic/model-b");
            assert_eq!(policies[1].caller_override, None);
            assert!(!policies[1].configured.contains_key(&key_a));
            assert_eq!(
                policies[1].configured.get(&key_b).map(String::as_str),
                Some("ultra")
            );
            assert!(policies[1].surfaces.values().any(|surface| {
                surface.supported.len() == 1 && surface.supported[0].value == "ultra"
            }));
        }
        assert_eq!(
            refresh_turn_configuration(&deps, None)
                .await
                .meta
                .context_window,
            222_222
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
        let mut deps = deps_at(dir.path(), llm.clone()).await;
        deps.model = None;
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
                    effort: None,
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
            display_name: None,
            reasoning_efforts: vec![],
            default_reasoning_effort: None,
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
        let cfg = ContextConfig::load(&deps.store, deps.meta.clone()).await;
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
        let cfg = ContextConfig::load(&deps.store, deps.meta.clone()).await;
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
    async fn emit_context_usage_reports_cache_creation() {
        let dir = tempfile::tempdir().unwrap();
        let llm: Arc<dyn LlmStream> = Arc::new(ScriptedLlm::new(vec![]));
        let deps = deps_at(dir.path(), llm).await;
        let cfg = ContextConfig::load(&deps.store, deps.meta.clone()).await;
        let mut cm = ContextManager::load(deps.store.clone(), &deps.session_pk, cfg)
            .await
            .unwrap();
        cm.observe_message_start(&json!({
            "usage": { "input_tokens": 30_000, "cache_creation_input_tokens": 12_000 }
        }));
        cm.commit_response();

        let mut rx = deps.events.subscribe();
        emit_context_usage(&deps, &cm, true).await;

        let mut creation = None;
        while let Ok(ev) = rx.try_recv() {
            if let CoreEvent::ContextUsage {
                cache_creation_tokens,
                ..
            } = ev
            {
                creation = Some(cache_creation_tokens);
                break;
            }
        }
        assert_eq!(
            creation,
            Some(12_000),
            "cache_creation must be surfaced on the event"
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
    async fn manual_compact_refreshes_turn_configuration_before_utility_call() {
        let dir = tempfile::tempdir().unwrap();
        let summarize = vec![text_delta("manual summary"), message_stop()];
        let llm = Arc::new(RecordingLlm::new(vec![summarize]));
        let mut deps = deps_at(dir.path(), llm.clone()).await;
        deps.project_id = Some("p".into());
        add_anthropic_conn(&deps.store, &["model-a"]).await;
        seed_pinned_project(&deps.store, Some("anthropic/model-a")).await;
        deps.store
            .update_project_runtime("p", Some("anthropic/model-a".into()), Some("high".into()))
            .await
            .unwrap();
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
        {
            let policies = llm.policies.lock().unwrap();
            assert_eq!(policies.len(), 1, "manual compact makes one utility call");
            assert_eq!(policies[0].requested_model, "anthropic/model-a");
            assert_eq!(policies[0].caller_override.as_deref(), Some("high"));
        }
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
            display_name: None,
            reasoning_efforts: vec![],
            default_reasoning_effort: None,
            cost_input: 0.0,
            cost_output: 0.0,
            cost_cache_read: 0.0,
            cost_cache_write: 0.0,
        };
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
        // Effort is now applied by the router against the immutable turn
        // policy, never reduced to a synthetic thinking budget in the runner.
        assert!(body.get("thinking").is_none());
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
        let llm = Arc::new(RecordingLlm::new(vec![turn1, turn2]));
        let deps = deps_at(dir.path(), llm.clone()).await;
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

        {
            let policies = llm.policies.lock().unwrap();
            assert_eq!(policies.len(), 2);
            assert!(Arc::ptr_eq(&policies[0], &policies[1]));
        }

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
    async fn dispatch_link_task_foreground_persists_tool_identity() {
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
        let children = deps
            .store
            .list_descendant_agent_runs(&deps.run_id)
            .await
            .unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].source_tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(children[0].dispatch_index, Some(0));
    }

    #[tokio::test]
    async fn dispatch_link_admission_failure_leaves_no_child_and_records_tool_error() {
        let dir = tempfile::tempdir().unwrap();
        let parent = vec![
            tool_use_start(0, "capacity-tool-call", "task"),
            input_json_delta(
                0,
                r#"{"subagent_type":"explore","prompt":"must not be admitted"}"#,
            ),
            message_delta("tool_use"),
            message_stop(),
        ];
        let parent_end = vec![
            text_delta("handled"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![parent, parent_end]));
        let deps = deps_at(dir.path(), llm).await;
        for index in 0..crate::delegation::MAX_ACTIVE_CHILD_RUNS {
            deps.delegation
                .queue_subagent(SubagentRunRequest {
                    parent_run_id: deps.run_id.clone(),
                    subagent_type: "explore".into(),
                    task: format!("existing-{index}"),
                    context: None,
                    background: false,
                    dispatch: None,
                })
                .await
                .unwrap();
        }

        run_turn(
            &deps,
            TurnPrompt::text("dispatch", "dispatch"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let children = deps
            .store
            .list_descendant_agent_runs(&deps.run_id)
            .await
            .unwrap();
        assert_eq!(children.len(), crate::delegation::MAX_ACTIVE_CHILD_RUNS);
        assert!(children
            .iter()
            .all(|child| child.source_tool_call_id.as_deref() != Some("capacity-tool-call")));
        let row = deps
            .store
            .list_messages(&deps.session_pk)
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.tool_call_id.as_deref() == Some("capacity-tool-call"))
            .expect("the terminal task tool row is persisted");
        assert_eq!(row.status.as_deref(), Some("failed"));
        assert!(row.payload["output"]
            .as_str()
            .expect("tool output")
            .contains("active child run limit"));
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
    fn delegated_main_child_filter_adds_only_delegation_tools() {
        use super::super::agents::ToolFilter;

        let names = ["read", "bash", "task", "delegate_agent", "write"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let filter = effective_delegated_main_child_filter(
            ToolFilter::Only(vec!["read".to_string()]),
            &names,
        );

        assert!(filter.allows("read"));
        assert!(filter.allows("task"));
        assert!(filter.allows("delegate_agent"));
        assert!(!filter.allows("bash"));
        assert!(!filter.allows("write"));
    }

    #[test]
    fn is_hot_classifies_core_vs_deferred() {
        for n in [
            "read",
            "ls",
            "glob",
            "grep",
            "bash",
            "edit",
            "write",
            "todowrite",
            "todoread",
            "skill",
            "task",
        ] {
            assert!(is_hot(n), "{n} must be hot");
        }
        for n in [
            "webfetch",
            "memory",
            "lsp",
            "session_search",
            "mcp__srv__do",
        ] {
            assert!(!is_hot(n), "{n} must be deferred");
        }
    }

    #[test]
    fn visible_tool_defs_none_is_the_full_filtered_set() {
        let tools = ToolRegistry::builtin();
        let agent = AgentRegistry::builtin().default_agent(); // tools: ToolFilter::All
        let eager = visible_tool_defs(&tools, &agent, None);
        // Full filtered set, no load_tools.
        let names: Vec<String> = eager
            .iter()
            .filter_map(|d| d["name"].as_str().map(String::from))
            .collect();
        assert!(names.contains(&"webfetch".to_string()));
        assert!(names.contains(&"read".to_string()));
        assert!(
            !names.iter().any(|n| n == LOAD_TOOLS_NAME),
            "eager mode has no synthetic load_tools"
        );
    }

    #[test]
    fn visible_tool_defs_lazy_hides_deferred_and_adds_load_tools() {
        let tools = ToolRegistry::builtin();
        let agent = AgentRegistry::builtin().default_agent();
        let empty = std::collections::BTreeSet::new();
        let lazy = visible_tool_defs(&tools, &agent, Some(&empty));
        let names: Vec<String> = lazy
            .iter()
            .filter_map(|d| d["name"].as_str().map(String::from))
            .collect();
        // Hot core present, deferred hidden, load_tools present and last.
        assert!(names.contains(&"read".to_string()));
        assert!(names.contains(&"bash".to_string()));
        assert!(
            !names.contains(&"webfetch".to_string()),
            "deferred hidden until loaded"
        );
        assert!(!names.contains(&"memory".to_string()));
        assert_eq!(names.last().map(String::as_str), Some(LOAD_TOOLS_NAME));
        // load_tools description lists the deferred tools by name.
        let lt = lazy.iter().find(|d| d["name"] == LOAD_TOOLS_NAME).unwrap();
        let desc = lt["description"].as_str().unwrap();
        assert!(
            desc.contains("webfetch"),
            "index must name deferred webfetch"
        );
        assert!(
            !desc.contains("\n- read:"),
            "hot tools are not in the load index"
        );
    }

    #[test]
    fn visible_tool_defs_lazy_reveals_an_activated_tool() {
        let tools = ToolRegistry::builtin();
        let agent = AgentRegistry::builtin().default_agent();
        let mut set = std::collections::BTreeSet::new();
        set.insert("webfetch".to_string());
        let lazy = visible_tool_defs(&tools, &agent, Some(&set));
        let names: Vec<String> = lazy
            .iter()
            .filter_map(|d| d["name"].as_str().map(String::from))
            .collect();
        assert!(
            names.contains(&"webfetch".to_string()),
            "activated tool is advertised in full"
        );
        // …and no longer in the load_tools index.
        let lt = lazy.iter().find(|d| d["name"] == LOAD_TOOLS_NAME).unwrap();
        assert!(!lt["description"]
            .as_str()
            .unwrap()
            .contains("\n- webfetch:"));
        // Deterministic order across calls.
        let again = visible_tool_defs(&tools, &agent, Some(&set));
        assert_eq!(lazy, again);
    }

    #[tokio::test]
    async fn primary_deps_advertise_hot_core_and_load_tools_only() {
        let dir = tempfile::tempdir().unwrap();
        let llm = std::sync::Arc::new(ScriptedLlm::new(vec![]));
        let mut deps = deps_at(dir.path(), llm).await;
        // Primary session: lazy tools on.
        deps.activated_tools = Some(std::sync::Arc::new(tokio::sync::Mutex::new(
            std::collections::BTreeSet::new(),
        )));
        let defs = current_tool_defs(&deps, &deps.agent).await;
        let names: Vec<String> = defs
            .iter()
            .filter_map(|d| d["name"].as_str().map(String::from))
            .collect();
        assert!(names.contains(&"read".to_string()));
        assert!(names.contains(&LOAD_TOOLS_NAME.to_string()));
        assert!(
            !names.contains(&"webfetch".to_string()),
            "deferred hidden for primary until loaded"
        );

        // Eager (sub-agent style): full set, no load_tools.
        deps.activated_tools = None;
        let eager = current_tool_defs(&deps, &deps.agent).await;
        let enames: Vec<String> = eager
            .iter()
            .filter_map(|d| d["name"].as_str().map(String::from))
            .collect();
        assert!(enames.contains(&"webfetch".to_string()));
        assert!(!enames.contains(&LOAD_TOOLS_NAME.to_string()));
    }

    #[tokio::test]
    async fn load_tools_reveals_a_deferred_tool_on_the_next_turn() {
        let dir = tempfile::tempdir().unwrap();
        // Turn 1: the model calls load_tools(["webfetch"]).
        let turn1 = vec![
            message_start_with_usage(1_000, 0),
            tool_use_start(0, "c1", "load_tools"),
            input_json_delta(0, r#"{"names":["webfetch"]}"#),
            message_delta("tool_use"),
            message_stop(),
        ];
        // Turn 2: the model finishes.
        let turn2 = vec![
            message_start_with_usage(1_000, 0),
            text_delta("done"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = std::sync::Arc::new(RecordingLlm::new(vec![turn1, turn2]));
        let mut deps = deps_at(dir.path(), llm.clone()).await;
        deps.activated_tools = Some(std::sync::Arc::new(tokio::sync::Mutex::new(
            std::collections::BTreeSet::new(),
        )));

        run_turn(&deps, TurnPrompt::text("x", "x"), CancellationToken::new())
            .await
            .unwrap();

        let bodies = llm.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "two provider turns");
        let names_of = |b: &serde_json::Value| -> Vec<String> {
            b["tools"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|t| t["name"].as_str().map(String::from))
                .collect()
        };
        let t1 = names_of(&bodies[0]);
        let t2 = names_of(&bodies[1]);
        // Turn 1: hot core + load_tools, webfetch deferred.
        assert!(t1.contains(&"load_tools".to_string()));
        assert!(t1.contains(&"read".to_string()));
        assert!(
            !t1.contains(&"webfetch".to_string()),
            "webfetch deferred on turn 1"
        );
        // Turn 2: webfetch now advertised (loaded).
        assert!(
            t2.contains(&"webfetch".to_string()),
            "webfetch loaded on turn 2"
        );
    }

    #[tokio::test]
    async fn load_tools_rejects_unknown_names() {
        let dir = tempfile::tempdir().unwrap();
        let turn1 = vec![
            message_start_with_usage(1_000, 0),
            tool_use_start(0, "c1", "load_tools"),
            input_json_delta(0, r#"{"names":["not_a_tool"]}"#),
            message_delta("tool_use"),
            message_stop(),
        ];
        let turn2 = vec![
            message_start_with_usage(1_000, 0),
            text_delta("ok"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = std::sync::Arc::new(RecordingLlm::new(vec![turn1, turn2]));
        let mut deps = deps_at(dir.path(), llm.clone()).await;
        let set = std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::BTreeSet::new()));
        deps.activated_tools = Some(set.clone());

        run_turn(&deps, TurnPrompt::text("x", "x"), CancellationToken::new())
            .await
            .unwrap();

        // Nothing was activated; turn 2 still has no bogus tool.
        assert!(
            set.lock().await.is_empty(),
            "unknown name must not be activated"
        );
        let bodies = llm.bodies.lock().unwrap();
        let t2: Vec<String> = bodies[1]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str().map(String::from))
            .collect();
        assert!(!t2.contains(&"not_a_tool".to_string()));
    }

    #[tokio::test]
    async fn load_tools_rejects_empty_names() {
        let dir = tempfile::tempdir().unwrap();
        let turn1 = vec![
            message_start_with_usage(1_000, 0),
            tool_use_start(0, "c1", "load_tools"),
            input_json_delta(0, r#"{"names":[]}"#),
            message_delta("tool_use"),
            message_stop(),
        ];
        let turn2 = vec![
            message_start_with_usage(1_000, 0),
            text_delta("ok"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = std::sync::Arc::new(RecordingLlm::new(vec![turn1, turn2]));
        let mut deps = deps_at(dir.path(), llm.clone()).await;
        let set = std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::BTreeSet::new()));
        deps.activated_tools = Some(set.clone());

        run_turn(&deps, TurnPrompt::text("x", "x"), CancellationToken::new())
            .await
            .unwrap();

        // Nothing was activated; the empty request is rejected outright.
        assert!(
            set.lock().await.is_empty(),
            "empty names must not activate anything"
        );

        // The tool_result the model sees must explain the problem, not
        // falsely claim success while is_error is set.
        let bodies = llm.bodies.lock().unwrap();
        let messages = bodies[1]["messages"].as_array().unwrap();
        let last = messages.last().expect("at least one message");
        let rendered = serde_json::to_string(last).unwrap();
        assert!(
            rendered.contains("No tool names"),
            "message must explain the empty names error: {rendered}"
        );
        assert!(
            !rendered.contains("Loaded:"),
            "message must not claim success for empty names: {rendered}"
        );
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

    #[test]
    fn subagent_blocklist_blocks_skill_manage() {
        use super::super::agents::ToolFilter;
        let names: Vec<String> = ["read", "bash", "skill", "skill_manage"]
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
        // `skill` (read-only recall) stays available; `skill_manage` (a
        // filesystem write, Phase 4 Task 6) is a primary/review-fork
        // capability only, never a sub-agent's.
        assert!(eff.allows("skill"));
        assert!(!eff.allows("skill_manage"));
    }

    #[test]
    fn subagent_blocklist_blocks_app_tools() {
        use super::super::agents::ToolFilter;
        let names: Vec<String> = crate::harness::native::tools::APP_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect();
        let eff = effective_child_filter(
            &ToolFilter::All,
            &ToolFilter::All,
            &names,
            SUBAGENT_BLOCKLIST,
        );
        for t in crate::harness::native::tools::APP_TOOLS {
            assert!(!eff.allows(t), "sub-agents must not get {t}");
        }
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
            .set_setting(crate::domain::WriteOrigin::User, "max_concurrent_runs", "1")
            .await
            .unwrap();
        let spawner = RunnerSpawner {
            deps: deps.clone(),
            cancel: CancellationToken::new(),
            depth: 0,
            parent_run_id: deps.run_id.clone(),
        };
        let results = spawner
            .run_many(
                "test-tool-call",
                vec![
                    SubtaskSpec {
                        agent_type: "explore".into(),
                        prompt: "first".into(),
                    },
                    SubtaskSpec {
                        agent_type: "explore".into(),
                        prompt: "second".into(),
                    },
                ],
            )
            .await;
        assert_eq!(results.len(), 2);
        assert_eq!((results[0].index, results[1].index), (0, 1));
        assert!(results.iter().all(|r| r.status == SubtaskStatus::Completed));
        assert_eq!(results[0].report, "report A");
        assert_eq!(results[1].report, "report B");
    }

    #[tokio::test]
    async fn dispatch_link_indices_follow_input_when_children_finish_out_of_order() {
        let dir = tempfile::tempdir().unwrap();
        let deps = deps_at(dir.path(), Arc::new(ScriptedLlm::new(vec![]))).await;
        let root_run_id = deps.run_id.clone();
        let store = deps.store.clone();
        let mut children = Vec::new();
        for (dispatch_index, task) in ["first", "second", "third"].into_iter().enumerate() {
            children.push(
                deps.delegation
                    .queue_subagent(SubagentRunRequest {
                        parent_run_id: root_run_id.clone(),
                        subagent_type: "explore".into(),
                        task: task.into(),
                        context: None,
                        background: false,
                        dispatch: Some(crate::delegation::AgentDispatchLink {
                            source_tool_call_id: "out-of-order-tool-call".into(),
                            dispatch_index: i64::try_from(dispatch_index)
                                .expect("test index fits i64"),
                        }),
                    })
                    .await
                    .unwrap(),
            );
        }
        for child in [2, 0, 1] {
            deps.delegation
                .complete(&children[child].run.run_id, "done")
                .await
                .unwrap();
        }
        let mut children = store
            .list_descendant_agent_runs(&root_run_id)
            .await
            .unwrap();
        children.sort_by_key(|child| child.dispatch_index);
        assert_eq!(
            children
                .iter()
                .map(|child| child.dispatch_index)
                .collect::<Vec<_>>(),
            vec![Some(0), Some(1), Some(2)]
        );
        assert!(children.iter().all(|child| {
            child.source_tool_call_id.as_deref() == Some("out-of-order-tool-call")
        }));
        assert_eq!(
            children
                .iter()
                .map(|child| child.task.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second", "third"]
        );
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
            parent_run_id: deps.run_id.clone(),
            deps,
            cancel: CancellationToken::new(),
            depth: 0,
        };
        let specs: Vec<SubtaskSpec> = (0..3)
            .map(|i| SubtaskSpec {
                agent_type: "explore".into(),
                prompt: format!("job {i}"),
            })
            .collect();
        let results = spawner.run_many("test-tool-call", specs).await;
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
            parent_run_id: deps.run_id.clone(),
            deps,
            cancel: CancellationToken::new(),
            depth: 0,
        };
        let results = spawner
            .run_many(
                "test-tool-call",
                vec![
                    SubtaskSpec {
                        agent_type: "no-such-agent".into(),
                        prompt: "x".into(),
                    },
                    SubtaskSpec {
                        agent_type: "explore".into(),
                        prompt: "y".into(),
                    },
                ],
            )
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
            parent_run_id: deps.run_id.clone(),
            deps,
            cancel,
            depth: 0,
        };
        let results = spawner
            .run_many(
                "test-tool-call",
                vec![
                    SubtaskSpec {
                        agent_type: "explore".into(),
                        prompt: "a".into(),
                    },
                    SubtaskSpec {
                        agent_type: "explore".into(),
                        prompt: "b".into(),
                    },
                ],
            )
            .await;
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|r| r.status == SubtaskStatus::Interrupted));
    }

    #[tokio::test]
    async fn subagent_deps_never_inherit_parent_memory() {
        use crate::agents::knowledge::AgentKnowledgeStore;
        use crate::harness::native::memory::MemoryStore;
        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(testutil::RecordingLlm::new(vec![]));
        let mut parent = deps_at(dir.path(), llm).await;
        parent.memory = Some(Arc::new(
            MemoryStore::for_agent(
                Arc::new(AgentKnowledgeStore::new(dir.path().to_path_buf())),
                "ryuzi",
                None,
            )
            .unwrap(),
        ));
        let child = deps_for_subagent(&parent).await.unwrap();
        assert!(child.memory.is_none());
        assert!(child.attachments_dir.is_none());
        assert_eq!(child.work_dir, parent.work_dir);
        assert_eq!(child.project_id, parent.project_id);
        assert!(child.app_control.is_none());
    }

    #[tokio::test]
    async fn subagent_cannot_read_parent_attachments() {
        let dir = tempfile::tempdir().unwrap();
        let parent_attachments = tempfile::tempdir().unwrap();
        let attachment_dir = parent_attachments.path().to_path_buf();
        let attachment = attachment_dir.join("private.txt");
        tokio::fs::write(&attachment, "parent-only attachment")
            .await
            .unwrap();
        let child_turn = vec![
            tool_use_start(0, "read-parent-attachment", "read"),
            input_json_delta(
                0,
                &format!(
                    r#"{{"path":{}}}"#,
                    serde_json::to_string(&attachment).unwrap()
                ),
            ),
            message_delta("tool_use"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![child_turn, final_turn("done")]));
        let mut deps = deps_at(dir.path(), llm).await;
        deps.attachments_dir = Some(attachment_dir);
        let spawner = RunnerSpawner {
            deps: deps.clone(),
            cancel: CancellationToken::new(),
            depth: 0,
            parent_run_id: deps.run_id.clone(),
        };

        let result = spawner
            .run_many(
                "test-tool-call",
                vec![SubtaskSpec {
                    agent_type: "general".into(),
                    prompt: "try to read the parent attachment".into(),
                }],
            )
            .await;

        assert_eq!(result[0].status, SubtaskStatus::Completed);
        let rows = deps.store.list_messages(&deps.session_pk).await.unwrap();
        let read = rows
            .iter()
            .find(|row| row.tool_call_id.as_deref() == Some("read-parent-attachment"))
            .expect("the attempted child read is recorded");
        assert!(
            !read.payload["output"]
                .as_str()
                .is_some_and(|output| output.contains("parent-only attachment")),
            "the parent attachment content must never reach the subagent"
        );
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
        let mem = MemoryStore::for_agent(
            Arc::new(crate::agents::knowledge::AgentKnowledgeStore::new(
                memdir.path().to_path_buf(),
            )),
            "ryuzi",
            None,
        )
        .unwrap();
        mem.add(MemoryScope::Global, "remember: the repo uses bun")
            .await
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
        // Override the default fixture title so this test still exercises
        // title generation without replacing its durable session/root run.
        deps.store.clear_session_title("s1").await.unwrap();
        deps.store.set_session_project("s1", "p").await.unwrap();

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
            .set_setting(
                crate::domain::WriteOrigin::User,
                "agent.auto_continue_budget",
                "0",
            )
            .await
            .unwrap();
        let agent = deps.agent.clone();
        let mut cm = ContextManager::ephemeral(
            &deps.session_pk,
            ContextConfig::with_meta(deps.meta.clone()),
        );
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
            .set_setting(
                crate::domain::WriteOrigin::User,
                "agent.max_provider_turns",
                "1",
            )
            .await
            .unwrap();
        deps.store
            .set_setting(
                crate::domain::WriteOrigin::User,
                "agent.auto_continue_budget",
                "1",
            )
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
            .set_setting(
                crate::domain::WriteOrigin::User,
                "agent.max_provider_turns",
                "1",
            )
            .await
            .unwrap();
        deps.store
            .set_setting(
                crate::domain::WriteOrigin::User,
                "agent.auto_continue_budget",
                "0",
            )
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

    /// Redirect `dirs::data_dir()` into a tempdir for the duration of a test —
    /// `run_background`'s spill path (`paths::chat_scratch_dir`) resolves
    /// under the real state dir otherwise, which a test must never touch.
    /// Process-global env, so every test using this needs `#[serial]`
    /// (mirrors `harness::native::tests::StateDirGuard`).
    struct StateDirGuard {
        _dir: tempfile::TempDir,
    }
    impl StateDirGuard {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
            std::env::set_var("HOME", dir.path());
            StateDirGuard { _dir: dir }
        }
    }

    /// Ensure `session_pk` has a durable session row for tests that exercise
    /// background delivery. `deps_with_store_and_registry` already creates the
    /// matching owned root run used by run-scoped transcript emission.
    async fn seed_idle_session(store: &Store, session_pk: &str) {
        if store.get_session(session_pk).await.unwrap().is_some() {
            return;
        }
        use crate::domain::{Session, SessionKind, SessionStatus};
        store
            .insert_session(Session {
                session_pk: session_pk.to_string(),
                primary_agent_id: None,
                primary_agent_snapshot: None,
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: Some("bg-parent".into()),
                status: SessionStatus::Idle,
                perm_mode: PermMode::BypassPermissions,
                started_by: None,
                created_at: Some(0),
                last_active: Some(0),
                resume_attempts: 0,
                branch_owned: false,
                kind: SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
    }

    async fn wait_for_background_release(
        background: &crate::harness::native::background::BackgroundRegistry,
    ) {
        for _ in 0..200 {
            if background.active() == 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("background reservation was not released within the poll window");
    }

    /// Poll the rail (bounded) until the detached worker for `session_pk`
    /// writes its completion row, claiming (and thus returning) it.
    async fn wait_for_rail_row(store: &Store, session_pk: &str) -> crate::domain::BackgroundEvent {
        for _ in 0..200 {
            if let Some(row) = store
                .claim_deliverable_background_event("test-poll")
                .await
                .unwrap()
            {
                assert_eq!(row.target_session_pk, session_pk);
                return row;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("no rail row appeared for {session_pk} within the poll window");
    }

    #[tokio::test]
    async fn run_background_rejects_at_capacity_with_fallback_note() {
        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(ScriptedLlm::new(vec![]));
        let deps = deps_at(dir.path(), llm).await;
        deps.store
            .set_setting(crate::domain::WriteOrigin::User, "max_concurrent_runs", "1")
            .await
            .unwrap();
        let spawner = RunnerSpawner {
            deps: deps.clone(),
            cancel: CancellationToken::new(),
            depth: 0,
            parent_run_id: deps.run_id.clone(),
        };
        // Fill the one slot with a manual reservation.
        let _held = deps.background.try_reserve(1, &deps.session_pk).unwrap();
        let out = spawner
            .run_background(
                "test-tool-call",
                SubtaskSpec {
                    agent_type: "general".into(),
                    prompt: "do it".into(),
                },
            )
            .await;
        match out {
            BackgroundDispatch::Rejected { note } => {
                assert!(note.contains("capacity reached"));
                assert!(
                    note.contains("background=false"),
                    "teaches the sync fallback"
                );
            }
            _ => panic!("expected rejection at capacity"),
        }
        // Nothing was dispatched — capacity stays exactly as the manual hold left it.
        assert_eq!(deps.background.active(), 1);
    }

    #[tokio::test]
    async fn run_background_at_nonzero_depth_rejects_without_reserving() {
        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(ScriptedLlm::new(vec![]));
        let deps = deps_at(dir.path(), llm).await;
        let spawner = RunnerSpawner {
            deps: deps.clone(),
            cancel: CancellationToken::new(),
            depth: 1,
            parent_run_id: deps.run_id.clone(),
        };
        let out = spawner
            .run_background(
                "test-tool-call",
                SubtaskSpec {
                    agent_type: "general".into(),
                    prompt: "do it".into(),
                },
            )
            .await;
        match out {
            BackgroundDispatch::Rejected { note } => {
                assert!(note.contains("top level"));
            }
            _ => panic!("expected rejection at nonzero depth"),
        }
        assert_eq!(deps.background.active(), 0, "no reservation taken");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn nested_main_background_task_delivers_to_the_root_run() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::new();
        let child_turn = vec![
            text_delta("all done"),
            message_delta("end_turn"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![child_turn]));
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db.path()).await.unwrap());
        let (mut deps, registry) =
            deps_with_executable_profile_registry(dir.path(), llm, store).await;
        let target = registry
            .create(crate::agents::types::AgentMutationInput {
                name: "Delegate".into(),
                description: "delegate".into(),
                avatar: crate::agents::types::AgentAvatar {
                    color: "violet".into(),
                },
                model: crate::agents::types::AgentModel::Concrete {
                    name: "anthropic/target-model".into(),
                    effort: None,
                },
                permissions: crate::agents::types::AgentPermissions {
                    mode: PermMode::BypassPermissions,
                    rules: Vec::new(),
                },
                skills: Vec::new(),
                tools: crate::agents::types::AgentTools {
                    native: Vec::new(),
                    plugins: Vec::new(),
                    apps: Vec::new(),
                },
                loop_settings: crate::agents::types::AgentLoop {
                    max_turns: 1,
                    max_tool_rounds: 1,
                },
            })
            .await
            .unwrap();
        deps.model = Some("anthropic/model-a".into());
        // The parent session row must exist + be idle for the rail JOIN later.
        seed_idle_session(&deps.store, &deps.session_pk).await;
        let root = deps
            .delegation
            .begin_primary(&deps.session_pk, deps.primary_agent.clone(), "audit auth")
            .await
            .unwrap();
        let root_run_id = root.run.run_id.clone();
        let parent = deps
            .delegation
            .queue_main(crate::delegation::MainDelegationRequest {
                parent_run_id: root_run_id.clone(),
                target_agent_id: target.profile.id,
                task: "delegate audit".into(),
                context: None,
                background: false,
                dispatch: None,
            })
            .await
            .unwrap();
        deps.run_id = parent.run.run_id.clone();
        deps.root_run_id = root_run_id.clone();
        // Generous headroom so the short child report is not spilled.
        deps.store
            .upsert_session_context(
                &deps.session_pk,
                &json!({"usable_window": 100_000u64, "active_tokens": 0u64}),
            )
            .await
            .unwrap();
        let spawner = RunnerSpawner {
            deps: deps.clone(),
            cancel: CancellationToken::new(),
            depth: 0,
            parent_run_id: parent.run.run_id,
        };
        let out = spawner
            .run_background(
                "test-tool-call",
                SubtaskSpec {
                    agent_type: "general".into(),
                    prompt: "audit auth".into(),
                },
            )
            .await;
        let id = match out {
            BackgroundDispatch::Dispatched { id } => id,
            _ => panic!("expected dispatch"),
        };
        let row = wait_for_rail_row(&deps.store, &deps.session_pk).await;
        assert_eq!(row.kind, crate::domain::BackgroundKind::Delegation.as_str());
        assert_eq!(
            row.origin_run_id.as_deref(),
            Some(root_run_id.as_str()),
            "a main delegate's background task must deliver to the root primary run"
        );
        assert!(row
            .payload
            .contains(&format!("[ASYNC DELEGATION COMPLETE — {id}]")));
        assert!(row.payload.contains("all done"));
    }

    #[tokio::test]
    async fn background_main_delegate_reserves_a_cancellable_worker_and_never_enqueues_after_end() {
        use crate::agents::types::{
            AgentAvatar, AgentLoop, AgentModel, AgentMutationInput, AgentPermissions, AgentTools,
        };

        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(ScriptedLlm::new(vec![]));
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db.path()).await.unwrap());
        let (deps, registry) = deps_with_executable_profile_registry(dir.path(), llm, store).await;
        seed_idle_session(&deps.store, &deps.session_pk).await;
        let target = registry
            .create(AgentMutationInput {
                name: "Background target".into(),
                description: "background target".into(),
                avatar: AgentAvatar {
                    color: "violet".into(),
                },
                model: AgentModel::Concrete {
                    name: "anthropic/target-model".into(),
                    effort: None,
                },
                permissions: AgentPermissions {
                    mode: PermMode::BypassPermissions,
                    rules: Vec::new(),
                },
                skills: Vec::new(),
                tools: AgentTools {
                    native: Vec::new(),
                    plugins: Vec::new(),
                    apps: Vec::new(),
                },
                loop_settings: AgentLoop {
                    max_turns: 1,
                    max_tool_rounds: 1,
                },
            })
            .await
            .unwrap();
        let root_run_id = deps.run_id.clone();

        let dispatched = RunnerMainAgentSpawner { deps: deps.clone() }
            .run_child(crate::delegation::MainDelegationRequest {
                parent_run_id: root_run_id,
                target_agent_id: target.profile.id,
                task: "wait for cancellation".into(),
                context: None,
                background: true,
                dispatch: None,
            })
            .await;
        assert_eq!(
            deps.background.active(),
            1,
            "main delegates reserve background capacity"
        );

        deps.background.interrupt_for_session(&deps.session_pk);
        wait_for_background_release(&deps.background).await;
        assert_eq!(
            deps.background.active(),
            0,
            "cancellation releases the main delegate reservation"
        );
        assert_eq!(
            deps.store.pending_background_count().await.unwrap(),
            0,
            "a cancelled main delegate cannot enqueue a stale rail row"
        );
        let child = deps
            .delegation
            .await_terminal(&dispatched.run_id)
            .await
            .expect("the cancelled worker records its terminal run");
        assert_eq!(
            child.status,
            crate::domain::AgentRunStatus::Cancelled,
            "cancelling the detached worker must mark its child run cancelled"
        );
    }

    #[tokio::test]
    async fn background_main_delegate_enqueues_and_delivers_on_the_delegation_rail() {
        use crate::agents::types::{
            AgentAvatar, AgentLoop, AgentModel, AgentMutationInput, AgentPermissions, AgentTools,
        };

        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(ScriptedLlm::new(vec![final_turn("background main result")]));
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db.path()).await.unwrap());
        let (deps, registry) = deps_with_executable_profile_registry(dir.path(), llm, store).await;
        let target = registry
            .create(AgentMutationInput {
                name: "Background target".into(),
                description: "background target".into(),
                avatar: AgentAvatar {
                    color: "violet".into(),
                },
                model: AgentModel::Concrete {
                    name: "anthropic/target-model".into(),
                    effort: None,
                },
                permissions: AgentPermissions {
                    mode: PermMode::BypassPermissions,
                    rules: Vec::new(),
                },
                skills: Vec::new(),
                tools: AgentTools {
                    native: Vec::new(),
                    plugins: Vec::new(),
                    apps: Vec::new(),
                },
                loop_settings: AgentLoop {
                    max_turns: 1,
                    max_tool_rounds: 1,
                },
            })
            .await
            .unwrap();
        let root_run_id = deps.run_id.clone();

        let dispatched = RunnerMainAgentSpawner { deps: deps.clone() }
            .run_child(crate::delegation::MainDelegationRequest {
                parent_run_id: root_run_id.clone(),
                target_agent_id: target.profile.id,
                task: "finish in the background".into(),
                context: None,
                background: true,
                dispatch: None,
            })
            .await;

        for _ in 0..200 {
            if deps
                .store
                .get_agent_run(&dispatched.run_id)
                .await
                .unwrap()
                .is_some_and(|run| run.status.is_terminal())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let child = deps
            .store
            .get_agent_run(&dispatched.run_id)
            .await
            .unwrap()
            .expect("background main delegate is durable");
        assert_eq!(child.status, crate::domain::AgentRunStatus::Completed);
        assert_eq!(child.result.as_deref(), Some("background main result"));
        assert!(
            deps.store
                .list_run_messages(&deps.session_pk, &root_run_id)
                .await
                .unwrap()
                .iter()
                .all(|message| message.block_type != "delegation_result"),
            "main background results must not bypass the rail with a run message"
        );

        let claimed = wait_for_rail_row(&deps.store, &deps.session_pk).await;
        assert_eq!(
            claimed.kind,
            crate::domain::BackgroundKind::Delegation.as_str()
        );
        assert!(claimed.payload.contains(&dispatched.run_id));
        assert!(claimed.payload.contains("background main result"));
        assert_eq!(claimed.claimed_by.as_deref(), Some("test-poll"));
        deps.store
            .mark_background_delivered(&claimed.id)
            .await
            .unwrap();
        assert!(
            deps.store
                .claim_deliverable_background_event("after-delivery")
                .await
                .unwrap()
                .is_none(),
            "a delivered main delegation rail row is not claimed again"
        );
        assert!(
            deps.store
                .list_provider_turns(&deps.session_pk)
                .await
                .unwrap()
                .is_empty(),
            "enqueueing does not create a new primary turn; the generic rail owns delivery"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn continued_second_turn_background_main_and_task_rails_use_second_root() {
        use crate::agents::types::{
            AgentAvatar, AgentLoop, AgentModel, AgentMutationInput, AgentPermissions, AgentTools,
        };

        let _guard = StateDirGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(ScriptedLlm::new(vec![
            final_turn("background main result"),
            final_turn("background task result"),
        ]));
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db.path()).await.unwrap());
        let (mut deps, registry) =
            deps_with_executable_profile_registry(dir.path(), llm, store).await;
        let first_root = deps.root_run_id.clone();
        let second = deps
            .delegation
            .begin_primary(&deps.session_pk, deps.primary_agent.clone(), "second turn")
            .await
            .unwrap();
        let second_root = second.run.run_id;
        deps.run_id = second_root.clone();
        deps.root_run_id = second_root.clone();

        let target = registry
            .create(AgentMutationInput {
                name: "Background target".into(),
                description: "background target".into(),
                avatar: AgentAvatar {
                    color: "violet".into(),
                },
                model: AgentModel::Concrete {
                    name: "anthropic/target-model".into(),
                    effort: None,
                },
                permissions: AgentPermissions {
                    mode: PermMode::BypassPermissions,
                    rules: Vec::new(),
                },
                skills: Vec::new(),
                tools: AgentTools {
                    native: Vec::new(),
                    plugins: Vec::new(),
                    apps: Vec::new(),
                },
                loop_settings: AgentLoop {
                    max_turns: 1,
                    max_tool_rounds: 1,
                },
            })
            .await
            .unwrap();

        let main = RunnerMainAgentSpawner { deps: deps.clone() }
            .run_child(crate::delegation::MainDelegationRequest {
                parent_run_id: second_root.clone(),
                target_agent_id: target.profile.id,
                task: "delegate in the background".into(),
                context: None,
                background: true,
                dispatch: None,
            })
            .await;
        assert_eq!(main.status, SubtaskStatus::Completed);
        let task = RunnerSpawner {
            deps: deps.clone(),
            cancel: CancellationToken::new(),
            depth: 0,
            parent_run_id: second_root.clone(),
        }
        .run_background(
            "test-tool-call",
            SubtaskSpec {
                agent_type: "general".into(),
                prompt: "task in the background".into(),
            },
        )
        .await;
        assert!(matches!(task, BackgroundDispatch::Dispatched { .. }));

        let first_rail = wait_for_rail_row(&deps.store, &deps.session_pk).await;
        let second_rail = wait_for_rail_row(&deps.store, &deps.session_pk).await;
        for rail in [first_rail, second_rail] {
            assert_eq!(rail.origin_run_id.as_deref(), Some(second_root.as_str()));
            assert_ne!(rail.origin_run_id.as_deref(), Some(first_root.as_str()));
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn explicit_mention_nested_retry_background_rail_uses_outer_root_without_user_turn() {
        use crate::agents::types::{
            AgentAvatar, AgentLoop, AgentModel, AgentMutationInput, AgentPermissions, AgentTools,
        };

        let _guard = StateDirGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(ScriptedLlm::new(vec![final_turn(
            "background retry result",
        )]));
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db.path()).await.unwrap());
        let (mut deps, registry) =
            deps_with_executable_profile_registry(dir.path(), llm, store).await;
        let outer_root = deps.root_run_id.clone();
        let target = registry
            .create(AgentMutationInput {
                name: "Mentioned target".into(),
                description: "mentioned target".into(),
                avatar: AgentAvatar {
                    color: "violet".into(),
                },
                model: AgentModel::Concrete {
                    name: "anthropic/target-model".into(),
                    effort: None,
                },
                permissions: AgentPermissions {
                    mode: PermMode::BypassPermissions,
                    rules: Vec::new(),
                },
                skills: Vec::new(),
                tools: AgentTools {
                    native: Vec::new(),
                    plugins: Vec::new(),
                    apps: Vec::new(),
                },
                loop_settings: AgentLoop {
                    max_turns: 1,
                    max_tool_rounds: 1,
                },
            })
            .await
            .unwrap();
        let explicit_child = deps
            .delegation
            .queue_main(crate::delegation::MainDelegationRequest {
                parent_run_id: outer_root.clone(),
                target_agent_id: target.profile.id,
                task: "explicit mention".into(),
                context: None,
                background: false,
                dispatch: None,
            })
            .await
            .unwrap();
        let nested_child = deps
            .delegation
            .queue_subagent(SubagentRunRequest {
                parent_run_id: explicit_child.run.run_id.clone(),
                subagent_type: "general".into(),
                task: "nested task".into(),
                context: None,
                background: false,
                dispatch: None,
            })
            .await
            .unwrap();
        deps.delegation
            .fail(&explicit_child.run.run_id, "failed mention")
            .await
            .unwrap();
        deps.delegation
            .fail(&nested_child.run.run_id, "failed nested task")
            .await
            .unwrap();
        let main_retry = deps
            .delegation
            .retry_child_handle(&deps.session_pk, &explicit_child.run.run_id)
            .await
            .unwrap();
        let nested_retry = deps
            .delegation
            .retry_child_handle(&deps.session_pk, &nested_child.run.run_id)
            .await
            .unwrap();
        for retry in [&main_retry, &nested_retry] {
            assert_eq!(
                deps.store
                    .root_agent_run_id(&retry.run.run_id)
                    .await
                    .unwrap()
                    .as_deref(),
                Some(outer_root.as_str())
            );
        }

        deps.run_id = nested_retry.run.run_id.clone();
        deps.root_run_id = deps
            .store
            .root_agent_run_id(&nested_retry.run.run_id)
            .await
            .unwrap()
            .expect("nested retry has an outer primary root");
        let dispatched = RunnerSpawner {
            deps: deps.clone(),
            cancel: CancellationToken::new(),
            depth: 0,
            parent_run_id: nested_retry.run.run_id,
        }
        .run_background(
            "test-tool-call",
            SubtaskSpec {
                agent_type: "general".into(),
                prompt: "retry in the background".into(),
            },
        )
        .await;
        assert!(matches!(dispatched, BackgroundDispatch::Dispatched { .. }));

        let rail = wait_for_rail_row(&deps.store, &deps.session_pk).await;
        assert_eq!(rail.origin_run_id.as_deref(), Some(outer_root.as_str()));
        assert!(
            deps.store
                .list_messages(&deps.session_pk)
                .await
                .unwrap()
                .iter()
                .all(|message| message.role != "user"),
            "background delivery must stay on the rail instead of creating a user turn"
        );
    }

    #[tokio::test]
    async fn delegated_main_child_uses_the_target_profile_without_parent_leaks() {
        use crate::agents::types::{
            AgentAvatar, AgentLoop, AgentModel, AgentMutationInput, AgentPermissions, AgentTools,
            PermissionDecision, PermissionRule,
        };
        use testutil::RecordingLlm;

        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(RecordingLlm::new(vec![
            vec![
                tool_use_start(0, "target-app-call", "app_projects"),
                input_json_delta(0, r#"{"action":"list"}"#),
                message_delta("tool_use"),
                message_stop(),
            ],
            vec![
                tool_use_start(0, "target-app-call", "app_projects"),
                input_json_delta(0, r#"{"action":"list"}"#),
                message_delta("tool_use"),
                message_stop(),
            ],
            vec![
                tool_use_start(0, "target-mcp-call", "mcp__slack__send"),
                input_json_delta(0, r#"{}"#),
                message_delta("tool_use"),
                message_stop(),
            ],
            vec![
                tool_use_start(0, "profile-rule-call", "read"),
                input_json_delta(0, r#"{"path":"ignored-by-profile-rule"}"#),
                message_delta("tool_use"),
                message_stop(),
            ],
            final_turn("target done"),
        ]));
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db.path()).await.unwrap());
        let (mut deps, registry) =
            deps_with_executable_profile_registry(dir.path(), llm.clone(), store).await;
        let parent = registry
            .create(AgentMutationInput {
                name: "Parent".into(),
                description: "Parent-only profile".into(),
                avatar: AgentAvatar {
                    color: "orange".into(),
                },
                model: AgentModel::Concrete {
                    name: "anthropic/parent-model".into(),
                    effort: Some("low".into()),
                },
                permissions: AgentPermissions {
                    mode: PermMode::BypassPermissions,
                    rules: vec![PermissionRule {
                        id: "parent-rule".into(),
                        tool: "write".into(),
                        decision: PermissionDecision::Allow,
                        command_prefix: None,
                    }],
                },
                skills: vec!["parent-skill".into()],
                tools: AgentTools {
                    native: vec!["write".into()],
                    plugins: vec![],
                    apps: vec![],
                },
                loop_settings: AgentLoop {
                    max_turns: 9,
                    max_tool_rounds: 9,
                },
            })
            .await
            .unwrap();
        let target = registry
            .create(AgentMutationInput {
                name: "Target".into(),
                description: "Target-only profile".into(),
                avatar: AgentAvatar {
                    color: "violet".into(),
                },
                model: AgentModel::Concrete {
                    name: "anthropic/target-model".into(),
                    effort: Some("high".into()),
                },
                permissions: AgentPermissions {
                    mode: PermMode::BypassPermissions,
                    rules: vec![PermissionRule {
                        id: "target-rule".into(),
                        tool: "read".into(),
                        decision: PermissionDecision::Deny,
                        command_prefix: None,
                    }],
                },
                skills: vec!["target-skill".into()],
                tools: AgentTools {
                    native: vec!["read".into(), "bash".into(), "app_projects".into()],
                    plugins: vec!["github.search".into(), "lint.check".into()],
                    apps: vec!["slack".into()],
                },
                loop_settings: AgentLoop {
                    max_turns: 4,
                    max_tool_rounds: 1,
                },
            })
            .await
            .unwrap();
        deps.primary_agent = Arc::new(parent);
        deps.tools = Arc::new(ToolRegistry::with_extra(vec![
            Arc::new(crate::harness::native::tools::mcp::McpTool::new(
                "github",
                "search",
                "GitHub search",
                serde_json::json!({"type": "object"}),
                Arc::new(StaticMcpCaller),
                None,
            )),
            Arc::new(crate::harness::native::tools::mcp::McpTool::new(
                "slack",
                "send",
                "Slack send",
                serde_json::json!({"type": "object"}),
                Arc::new(StaticMcpCaller),
                None,
            )),
        ]));
        deps.app_control = Some(Arc::new(
            crate::harness::native::tools::testutil::FakeAppControl::default(),
        ));
        deps.attachments_dir = Some(dir.path().join("parent-attachments"));
        deps.memory = Some(Arc::new(
            crate::harness::native::memory::MemoryStore::for_agent(
                deps.agent_knowledge.clone(),
                "parent",
                None,
            )
            .unwrap(),
        ));
        seed_idle_session(&deps.store, &deps.session_pk).await;
        deps.store
            .set_setting_raw(
                "models.meta.anthropic/target-model",
                r#"{"context_window":222222,"max_output_tokens":3333}"#,
            )
            .await
            .unwrap();
        let root = deps
            .delegation
            .begin_primary(&deps.session_pk, deps.primary_agent.clone(), "parent")
            .await
            .unwrap();
        deps.run_id = root.run.run_id.clone();
        let parent_attachments = deps.attachments_dir.clone();
        let parent_memory = deps.memory.as_ref().unwrap().knowledge_root().to_path_buf();
        let target_memory = crate::harness::native::memory::MemoryStore::for_agent(
            deps.agent_knowledge.clone(),
            &target.profile.id,
            None,
        )
        .unwrap();
        target_memory
            .add(
                crate::harness::native::memory::MemoryScope::Global,
                "target memory only",
            )
            .await
            .unwrap();

        let result = RunnerMainAgentSpawner { deps: deps.clone() }
            .run_child(crate::delegation::MainDelegationRequest {
                parent_run_id: root.run.run_id,
                target_agent_id: target.profile.id.clone(),
                task: "inspect the target profile".into(),
                context: Some("only inspect authentication files".into()),
                background: false,
                dispatch: None,
            })
            .await;

        assert_eq!(result.status, SubtaskStatus::Completed);
        let child = deps
            .store
            .get_agent_run(&result.run_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            child.executing_agent_id.as_deref(),
            Some(target.profile.id.as_str())
        );
        assert_eq!(
            child.resolved_model.as_deref(),
            Some("anthropic/target-model")
        );
        assert_eq!(
            child.tool_count, 3,
            "only target-owned, schema-admitted tool calls are counted; the facade-gated app attempt is rejected before admission"
        );
        let bodies = llm.bodies.lock().unwrap().clone();
        assert_eq!(
            bodies.len(),
            5,
            "the target loop executes its configured turns"
        );
        let body = &bodies[0];
        assert_eq!(body["model"], "anthropic/target-model");
        assert_eq!(
            body["max_tokens"], 3_333,
            "the target model metadata controls its output context"
        );
        assert_eq!(
            llm.policies.lock().unwrap()[0].caller_override.as_deref(),
            Some("high")
        );
        let tool_rows = deps.store.list_messages(&deps.session_pk).await.unwrap();
        let target_app_call = tool_rows
            .iter()
            .find(|row| row.tool_call_id.as_deref() == Some("target-app-call"))
            .expect("target app facade call is recorded");
        assert_eq!(
            target_app_call.status.as_deref(),
            Some("failed"),
            "the target's app tool is advertised but cannot reach the parent's facade"
        );
        assert!(
            target_app_call.payload["output"]
                .as_str()
                .is_some_and(|output| output.contains("not available in this context")),
            "the target must not execute against the parent's app facade"
        );
        let target_mcp_call = tool_rows
            .iter()
            .find(|row| row.tool_call_id.as_deref() == Some("target-mcp-call"))
            .expect("target app MCP call is recorded");
        assert_eq!(target_mcp_call.status.as_deref(), Some("completed"));
        let profile_rule_call = tool_rows
            .iter()
            .find(|row| row.tool_call_id.as_deref() == Some("profile-rule-call"))
            .expect("target profile-rule call is recorded");
        assert_eq!(profile_rule_call.status.as_deref(), Some("failed"));
        assert_eq!(
            profile_rule_call.payload["output"], "Denied by user",
            "the target profile's deny rule applies even to a plan-safe read"
        );
        assert!(
            tool_rows.iter().all(|row| !matches!(
                row.tool_call_id.as_deref(),
                Some("plan-mode-call" | "task-call" | "delegate-agent-call")
            )),
            "parent-only bash and delegation calls must not leak into the target loop"
        );
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["text"], "inspect the target profile");
        assert_eq!(content[1]["text"], "only inspect authentication files");
        let system = body["system"].as_str().unwrap();
        assert!(system.contains("target memory only"));
        assert!(!system.contains("parent-skill"));
        let advertised = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert!(advertised.contains(&"read"));
        assert!(advertised.contains(&"bash"));
        assert!(advertised.contains(&"task"));
        assert!(advertised.contains(&"delegate_agent"));
        assert!(advertised.contains(&"mcp__github__search"));
        assert!(advertised.contains(&"mcp__slack__send"));
        assert!(advertised.contains(&"app_projects"));
        assert!(!advertised.contains(&"write"));
        assert!(!advertised.contains(&"ext__lint__check"));
        assert_eq!(
            parent_attachments,
            Some(dir.path().join("parent-attachments"))
        );
        assert_eq!(
            deps.memory.as_ref().unwrap().knowledge_root(),
            parent_memory
        );
        assert_ne!(target_memory.knowledge_root(), parent_memory.as_path());
    }

    #[tokio::test]
    async fn main_delegate_retry_uses_the_target_profile_runner() {
        use crate::agents::types::{
            AgentAvatar, AgentLoop, AgentModel, AgentMutationInput, AgentPermissions, AgentTools,
        };
        use testutil::RecordingLlm;

        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(RecordingLlm::new(vec![final_turn("target retry complete")]));
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db.path()).await.unwrap());
        let (mut deps, registry) =
            deps_with_executable_profile_registry(dir.path(), llm.clone(), store).await;
        let target = registry
            .create(AgentMutationInput {
                name: "Restricted target".into(),
                description: "target profile".into(),
                avatar: AgentAvatar {
                    color: "violet".into(),
                },
                model: AgentModel::Concrete {
                    name: "anthropic/target-model".into(),
                    effort: Some("high".into()),
                },
                permissions: AgentPermissions {
                    mode: PermMode::BypassPermissions,
                    rules: Vec::new(),
                },
                skills: Vec::new(),
                tools: AgentTools {
                    native: vec!["read".into()],
                    plugins: vec!["github.search".into()],
                    apps: vec!["slack".into()],
                },
                loop_settings: AgentLoop {
                    max_turns: 1,
                    max_tool_rounds: 1,
                },
            })
            .await
            .unwrap();
        deps.tools = Arc::new(ToolRegistry::with_extra(vec![
            Arc::new(crate::harness::native::tools::mcp::McpTool::new(
                "github",
                "search",
                "GitHub search",
                serde_json::json!({"type": "object"}),
                Arc::new(StaticMcpCaller),
                None,
            )),
            Arc::new(crate::harness::native::tools::mcp::McpTool::new(
                "slack",
                "send",
                "Slack send",
                serde_json::json!({"type": "object"}),
                Arc::new(StaticMcpCaller),
                None,
            )),
        ]));
        let failed = deps
            .delegation
            .queue_main(crate::delegation::MainDelegationRequest {
                parent_run_id: deps.run_id.clone(),
                target_agent_id: target.profile.id.clone(),
                task: "retry only this target task".into(),
                context: None,
                background: false,
                dispatch: None,
            })
            .await
            .unwrap();
        deps.delegation
            .fail(&failed.run.run_id, "failed")
            .await
            .unwrap();
        let retry = deps
            .delegation
            .retry_child_handle(&deps.session_pk, &failed.run.run_id)
            .await
            .unwrap();

        let retry_id = retry.run.run_id.clone();
        dispatch_retry_main_delegate(deps.clone(), retry).unwrap();
        let terminal = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            deps.delegation.await_terminal(&retry_id),
        )
        .await
        .expect("retry target must finish")
        .unwrap();

        assert_eq!(terminal.status, crate::domain::AgentRunStatus::Completed);
        let body = llm.bodies.lock().unwrap().pop().unwrap();
        assert_eq!(body["model"], "anthropic/target-model");
        assert_eq!(
            body["messages"][0]["content"][0]["text"],
            "retry only this target task"
        );
        let advertised = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert!(advertised.contains(&"read"));
        assert!(advertised.contains(&"task"));
        assert!(advertised.contains(&"delegate_agent"));
        assert!(advertised.contains(&"mcp__github__search"));
        assert!(advertised.contains(&"mcp__slack__send"));
        assert!(!advertised.contains(&"bash"));
        assert!(!advertised.contains(&"write"));
    }

    #[tokio::test]
    async fn tool_counts_include_main_subagent_and_retry_but_not_denied_or_unknown_calls() {
        let dir = tempfile::tempdir().unwrap();
        let main_allowed = vec![
            tool_use_start(0, "main-allowed", "bash"),
            input_json_delta(0, r#"{"command":"echo main"}"#),
            message_delta("tool_use"),
            message_stop(),
        ];
        let main_denied = vec![
            tool_use_start(0, "main-denied", "write"),
            input_json_delta(0, r#"{"path":"denied.txt","content":"no"}"#),
            message_delta("tool_use"),
            message_stop(),
        ];
        let main_unknown = vec![
            tool_use_start(0, "main-unknown", "unknown"),
            input_json_delta(0, "{}"),
            message_delta("tool_use"),
            message_stop(),
        ];
        let child_allowed = vec![
            tool_use_start(0, "child-allowed", "bash"),
            input_json_delta(0, r#"{"command":"echo child"}"#),
            message_delta("tool_use"),
            message_stop(),
        ];
        let retry_allowed = vec![
            tool_use_start(0, "retry-allowed", "bash"),
            input_json_delta(0, r#"{"command":"echo retry"}"#),
            message_delta("tool_use"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![
            main_allowed,
            main_denied,
            main_unknown,
            final_turn("main done"),
            child_allowed,
            vec![error_event("child failed")],
            retry_allowed,
            final_turn("retry done"),
        ]));
        let deps = deps_at(dir.path(), llm).await;
        let mut restricted = deps.agent.clone();
        restricted.tools = crate::harness::native::agents::ToolFilter::Only(vec!["bash".into()]);
        let budget = IterationBudget::new(4);
        let mut cm = ContextManager::ephemeral(
            &deps.session_pk,
            ContextConfig::with_meta(deps.meta.clone()),
        );
        cm.append_user(json!([{ "type": "text", "text": "count tools" }]))
            .await
            .unwrap();
        drive(
            &deps,
            &restricted,
            &mut cm,
            &CancellationToken::new(),
            None,
            DisplayMode::Full,
            &budget,
        )
        .await
        .unwrap();
        assert_eq!(
            deps.store
                .get_agent_run(&deps.run_id)
                .await
                .unwrap()
                .unwrap()
                .tool_count,
            1,
            "the primary run counts only its allowed known call"
        );

        let spawner = RunnerSpawner {
            deps: deps.clone(),
            cancel: CancellationToken::new(),
            depth: 0,
            parent_run_id: deps.run_id.clone(),
        };
        let child = spawner
            .run_many(
                "test-tool-call",
                vec![SubtaskSpec {
                    agent_type: "general".into(),
                    prompt: "run the child tool".into(),
                }],
            )
            .await;
        assert_eq!(child[0].status, SubtaskStatus::Error);
        let first_child = deps
            .store
            .list_descendant_agent_runs(&deps.run_id)
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(first_child.tool_count, 1);

        let retry = deps
            .delegation
            .retry_child(&deps.session_pk, &first_child.run_id)
            .await
            .unwrap();
        let retry_handle = crate::delegation::RunHandle {
            run: retry.clone(),
            agent_snapshot: None,
            cancel: CancellationToken::new(),
        };
        let retry_spawner = RunnerSpawner {
            deps: deps.clone(),
            cancel: CancellationToken::new(),
            depth: 0,
            parent_run_id: deps.run_id.clone(),
        };
        let retry_result = retry_spawner
            .run_queued_child(
                0,
                SubtaskSpec {
                    agent_type: "general".into(),
                    prompt: "retry the child tool".into(),
                },
                retry_handle.cancel.clone(),
                retry_handle,
            )
            .await;
        assert_eq!(retry_result.status, SubtaskStatus::Completed);
        assert_eq!(
            deps.store
                .get_agent_run(&retry.run_id)
                .await
                .unwrap()
                .unwrap()
                .tool_count,
            1,
            "the retry owns a new single allowed call"
        );
    }

    #[tokio::test]
    async fn cancelling_a_running_subagent_stops_follow_on_tools_and_preserves_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let effects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let blocking_turn = vec![
            tool_use_start(0, "blocking-call", "blocking"),
            input_json_delta(0, "{}"),
            message_delta("tool_use"),
            message_stop(),
        ];
        let next_tool_turn = vec![
            tool_use_start(0, "must-not-run", "bash"),
            input_json_delta(0, r#"{"command":"echo side-effect"}"#),
            message_delta("tool_use"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![blocking_turn, next_tool_turn]));
        let mut deps = deps_at(dir.path(), llm).await;
        deps.tools = Arc::new(ToolRegistry::with_extra(vec![Arc::new(BlockingTool {
            started: started.clone(),
            release: release.clone(),
            effects: effects.clone(),
        })]));
        let root = deps.run_id.clone();
        let spawner = RunnerSpawner {
            deps: deps.clone(),
            cancel: CancellationToken::new(),
            depth: 0,
            parent_run_id: root,
        };
        let worker = tokio::spawn(async move {
            spawner
                .run_many(
                    "test-tool-call",
                    vec![SubtaskSpec {
                        agent_type: "general".into(),
                        prompt: "block until cancelled".into(),
                    }],
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
            .await
            .expect("the child entered its blocking tool");
        let child = deps
            .store
            .list_descendant_agent_runs(&deps.run_id)
            .await
            .unwrap()
            .pop()
            .expect("the child is durably queued before it runs");
        assert_eq!(child.status, crate::domain::AgentRunStatus::Running);
        deps.delegation
            .cancel_child(&deps.session_pk, &child.run_id)
            .await
            .unwrap();
        release.notify_one();
        let results = tokio::time::timeout(std::time::Duration::from_secs(2), worker)
            .await
            .expect("cancelling a child must settle its worker")
            .unwrap();

        assert_eq!(results[0].status, SubtaskStatus::Interrupted);
        assert_eq!(effects.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            deps.store
                .get_agent_run(&child.run_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            crate::domain::AgentRunStatus::Cancelled,
            "the worker must not overwrite the runtime cancellation"
        );
        assert!(
            deps.store
                .list_messages(&deps.session_pk)
                .await
                .unwrap()
                .iter()
                .all(|row| row.tool_call_id.as_deref() != Some("must-not-run")),
            "the cancellation token stops the loop before a subsequent tool side effect"
        );
    }

    #[tokio::test]
    async fn isolated_main_target_executes_advertised_task_subagents() {
        let dir = tempfile::tempdir().unwrap();
        let parent_task = vec![
            tool_use_start(0, "delegate-work", "task"),
            input_json_delta(0, r#"{"subagent_type":"general","prompt":"inspect"}"#),
            message_delta("tool_use"),
            message_stop(),
        ];
        let llm = Arc::new(testutil::RecordingLlm::new(vec![
            parent_task,
            final_turn("subagent complete"),
            final_turn("parent complete"),
        ]));
        let mut deps = deps_at(dir.path(), llm.clone()).await;
        deps.isolated_target = true;

        run_turn(
            &deps,
            TurnPrompt::text("delegate", "delegate"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(llm.bodies.lock().unwrap().len(), 3);
        let children = deps
            .store
            .list_descendant_agent_runs(&deps.run_id)
            .await
            .unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].status, crate::domain::AgentRunStatus::Completed);
        assert_eq!(children[0].result.as_deref(), Some("subagent complete"));
    }

    #[tokio::test]
    async fn subagent_uses_current_shared_model_effort_and_audits_it() {
        use crate::agents::types::AgentModel;
        use testutil::RecordingLlm;

        let dir = tempfile::tempdir().unwrap();
        let llm = Arc::new(RecordingLlm::new(vec![final_turn("subagent complete")]));
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db.path()).await.unwrap());
        let (mut deps, registry) =
            deps_with_executable_profile_registry(dir.path(), llm.clone(), store).await;
        deps.model = Some("anthropic/parent-model".into());
        registry
            .set_subagent_model(AgentModel::Concrete {
                name: "anthropic/target-model".into(),
                effort: Some("high".into()),
            })
            .await
            .unwrap();
        let spawner = RunnerSpawner {
            deps: deps.clone(),
            cancel: CancellationToken::new(),
            depth: 0,
            parent_run_id: deps.run_id.clone(),
        };

        let result = spawner
            .run_many(
                "test-tool-call",
                vec![SubtaskSpec {
                    agent_type: "general".into(),
                    prompt: "inspect".into(),
                }],
            )
            .await;

        assert_eq!(result[0].status, SubtaskStatus::Completed);
        let body = llm.bodies.lock().unwrap().pop().unwrap();
        assert_eq!(body["model"], "anthropic/target-model");
        let policy = llm.policies.lock().unwrap().pop().unwrap();
        assert_eq!(policy.caller_override.as_deref(), Some("high"));
        let child = deps
            .store
            .list_descendant_agent_runs(&deps.run_id)
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(
            child.resolved_model.as_deref(),
            Some("anthropic/target-model")
        );
        assert_eq!(child.resolved_effort.as_deref(), Some("high"));
    }

    #[tokio::test]
    async fn task_children_are_durable_runs_with_tool_counts_and_terminal_results() {
        let dir = tempfile::tempdir().unwrap();
        let child_turn = vec![
            tool_use_start(0, "child-call", "bash"),
            input_json_delta(0, "{\"command\":\"echo child\"}"),
            message_delta("tool_use"),
            message_stop(),
        ];
        let llm = Arc::new(ScriptedLlm::new(vec![child_turn, final_turn("child done")]));
        let deps = deps_at(dir.path(), llm).await;
        // `deps_at` already owns a root run for run-scoped transcript rows.
        let root = deps.run_id.clone();
        let spawner = RunnerSpawner {
            deps: deps.clone(),
            cancel: CancellationToken::new(),
            depth: 0,
            parent_run_id: deps.run_id.clone(),
        };
        let results = spawner
            .run_many(
                "test-tool-call",
                vec![SubtaskSpec {
                    agent_type: "general".into(),
                    prompt: "inspect the workspace".into(),
                }],
            )
            .await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, SubtaskStatus::Completed);
        assert_eq!(results[0].report, "child done");
        let children = deps.store.list_descendant_agent_runs(&root).await.unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].status, crate::domain::AgentRunStatus::Completed);
        assert_eq!(children[0].tool_count, 1);
        assert_eq!(
            deps.store
                .list_run_messages(&deps.session_pk, &children[0].run_id)
                .await
                .unwrap()
                .len(),
            1,
            "the child tool call must be attached to its durable run"
        );
    }

    #[tokio::test]
    async fn run_background_cancelled_worker_writes_nothing_to_the_rail() {
        let dir = tempfile::tempdir().unwrap();
        // No scripted turns: the cancelled worker must never reach the model.
        let llm = Arc::new(ScriptedLlm::new(vec![]));
        let mut deps = deps_at(dir.path(), llm).await;
        seed_idle_session(&deps.store, &deps.session_pk).await;
        let root = deps
            .delegation
            .begin_primary(&deps.session_pk, deps.primary_agent.clone(), "audit auth")
            .await
            .unwrap();
        deps.run_id = root.run.run_id;
        let spawner = RunnerSpawner {
            deps: deps.clone(),
            cancel: CancellationToken::new(),
            depth: 0,
            parent_run_id: deps.run_id.clone(),
        };
        let out = spawner
            .run_background(
                "test-tool-call",
                SubtaskSpec {
                    agent_type: "general".into(),
                    prompt: "audit auth".into(),
                },
            )
            .await;
        let id = match out {
            BackgroundDispatch::Dispatched { id } => id,
            BackgroundDispatch::Rejected { note } => panic!("expected dispatch: {note}"),
        };
        let child = deps.store.get_agent_run(&id).await.unwrap().unwrap();
        assert_eq!(child.source_tool_call_id.as_deref(), Some("test-tool-call"));
        assert_eq!(child.dispatch_index, Some(0));
        // Single-threaded test runtime: the detached worker cannot have run
        // any code yet (no `.await` has yielded since `run_background`
        // returned), so this cancellation always lands before the worker
        // observes anything but a cancelled token.
        deps.background.interrupt_for_session(&deps.session_pk);
        // Let the detached task run to completion (or early-return).
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            deps.store.pending_background_count().await.unwrap(),
            0,
            "a cancelled worker must not write a stale completion to the rail"
        );
    }
}
