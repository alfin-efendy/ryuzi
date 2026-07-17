//! Built-in tool suite for the native runtime.
//!
//! Each [`Tool`] declares a name, a JSON-schema for its input (hand-written to
//! avoid a `schemars` dependency), a `tool_kind` for the Cockpit UI, a
//! per-call [`PermissionSpec`], and an async `execute`. The [`ToolRegistry`]
//! assembles the built-ins and produces the Anthropic `tools` array.
//!
//! All file-touching tools resolve paths through [`jail`], which confines them
//! to the session worktree, and cap their output via [`truncate`].

use crate::approval::ApprovalKey;
use crate::harness::native::capabilities::{ToolCapabilityProfile, ToolInteractionMode};
use crate::harness::native::tool_contract::{
    compile_canonical_schema, compile_openai_strict_schema, explicit_open_object_schema,
    AvailabilityProbe, NormalizedInput, PreflightMeta, ToolDescriptor, ToolError, ToolInputCtx,
    MAX_TOOL_DESCRIPTION_BYTES, MAX_TOOL_SCHEMA_BYTES,
};
use crate::store::Store;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub mod app_jobs;
pub mod app_projects;
pub mod bash;
pub mod clarify;
pub mod delegate;
pub mod edit;
pub mod extension;
pub mod glob;
pub mod grep;
pub mod ls;
pub mod lsp;
pub mod mcp;
pub mod memory;
pub mod plan;
pub mod question;
pub mod read;
pub mod revert;
pub mod session_search;
pub mod skill;
pub mod skill_manage;
pub mod task;
pub mod todo;
pub mod webfetch;
pub mod websearch;
pub mod write;

/// Bound on a single tool's model-visible output.
#[derive(Debug, Clone, Copy)]
pub struct OutputCaps {
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl Default for OutputCaps {
    fn default() -> Self {
        OutputCaps {
            max_lines: 2000,
            max_bytes: 50_000,
        }
    }
}

/// One delegated subtask in a `task` batch.
#[derive(Debug, Clone)]
pub struct SubtaskSpec {
    pub agent_type: String,
    pub prompt: String,
}

/// How one delegated subtask ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtaskStatus {
    Completed,
    Error,
    Interrupted,
}

impl SubtaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SubtaskStatus::Completed => "completed",
            SubtaskStatus::Error => "error",
            SubtaskStatus::Interrupted => "interrupted",
        }
    }
}

/// Outcome of one delegated subtask, ordered by `index` within its batch.
#[derive(Debug, Clone)]
pub struct SubtaskResult {
    pub index: usize,
    pub agent_type: String,
    pub status: SubtaskStatus,
    pub report: String,
}

/// Outcome of a `background: true` delegation dispatch (spec §6.2): either
/// accepted (the result will re-enter the chat via the rail) or rejected at
/// capacity (the caller falls back to a synchronous `task`).
#[derive(Debug, Clone)]
pub enum BackgroundDispatch {
    Dispatched { id: String },
    Rejected { note: String },
}

pub struct MainDelegationResult {
    pub run_id: String,
    pub agent_id: String,
    pub status: SubtaskStatus,
    pub report: String,
}

impl MainDelegationResult {
    pub fn completed(
        run_id: impl Into<String>,
        agent_id: impl Into<String>,
        report: impl Into<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            agent_id: agent_id.into(),
            status: SubtaskStatus::Completed,
            report: report.into(),
        }
    }
}

/// Spawns complete main-agent profiles for `delegate_agent`. It is intentionally
/// distinct from [`SubagentSpawner`]: main delegates retain their immutable
/// durable profile, while `task` children are bounded ephemeral subagents.
#[async_trait]
pub trait MainAgentSpawner: Send + Sync {
    /// `(id, name, description)` for executable profiles the current agent may
    /// delegate to. The runner excludes the caller and invalid profiles.
    async fn available(&self) -> Vec<(String, String, String)>;
    async fn run_one(
        &self,
        request: crate::delegation::MainDelegationRequest,
    ) -> MainDelegationResult;
    async fn run_many(
        &self,
        requests: Vec<crate::delegation::MainDelegationRequest>,
    ) -> Vec<MainDelegationResult>;
}

/// Spawns sub-agents for the `task` tool. Implemented by the runner; `None`
/// inside a sub-agent's own `ToolCtx` unless that agent may delegate.
#[async_trait]
pub trait SubagentSpawner: Send + Sync {
    /// Run a batch of subtasks concurrently (bounded by the
    /// `max_concurrent_runs` setting) and return one result per spec, ordered
    /// by index. Individual failures land in their entry — the batch itself
    /// never fails.
    async fn run_many(&self, specs: Vec<SubtaskSpec>) -> Vec<SubtaskResult>;
    /// Names of agents that may be spawned (for the tool description/errors).
    fn available(&self) -> Vec<String>;

    /// Run one `agent_type` on `prompt` to completion and return its final
    /// text — the single-task view over [`Self::run_many`].
    async fn run(&self, agent_type: &str, prompt: &str) -> anyhow::Result<String> {
        let mut results = self
            .run_many(vec![SubtaskSpec {
                agent_type: agent_type.to_string(),
                prompt: prompt.to_string(),
            }])
            .await;
        let r = results
            .pop()
            .ok_or_else(|| anyhow::anyhow!("spawner returned no result"))?;
        match r.status {
            SubtaskStatus::Completed => Ok(r.report),
            SubtaskStatus::Interrupted => anyhow::bail!("interrupted"),
            SubtaskStatus::Error => anyhow::bail!("{}", r.report),
        }
    }

    /// Dispatch one subtask to run in the BACKGROUND (does not block the
    /// parent turn); its result re-enters the parent chat via the rail. The
    /// default rejects — only the top-level runner spawner supports it.
    async fn run_background(&self, _spec: SubtaskSpec) -> BackgroundDispatch {
        BackgroundDispatch::Rejected {
            note: "background delegation is not available for this agent".to_string(),
        }
    }
}

/// A cron job as seen through the curated app surface.
#[derive(Debug, Clone)]
pub struct AppJobSummary {
    pub id: String,
    pub name: String,
    pub cron: String,
    pub enabled: bool,
}

/// Inputs to create a job through `app_jobs`. `schedule` is natural language
/// (`crate::scheduler::natural_to_cron`) or a raw cron expression.
#[derive(Debug, Clone)]
pub struct AppJobCreate {
    pub name: String,
    pub schedule: String,
    pub prompt: String,
    pub project_id: Option<String>,
    pub model_override: Option<String>,
}

/// A project as seen through `app_projects`.
#[derive(Debug, Clone)]
pub struct AppProjectSummary {
    pub id: String,
    pub name: String,
}

/// The curated surface the agent uses to operate the app itself (spec §9.1).
///
/// This is the ENTIRE app-control contract: what is not a method here is not a
/// capability the agent has. It is deliberately narrow — no settings, model
/// switching, approval resolution, daemon control, or OAuth (spec §9.3
/// negative space). Every mutating method records an audit row inside its
/// implementation, so auditing cannot be forgotten per-tool. `None` on
/// `ToolCtx` (sub-agents, workers, review forks, bare tests) means "no app
/// control"; the tool then returns a "not available" error.
#[async_trait]
pub trait AppControl: Send + Sync {
    /// The originating write origin (always `Agent` in production; used for the
    /// audit rows the impl writes).
    fn origin(&self) -> crate::domain::WriteOrigin;

    // --- jobs (permission keys jobs.read / jobs.write) ---
    async fn list_jobs(&self) -> anyhow::Result<Vec<AppJobSummary>>;
    async fn create_job(&self, spec: AppJobCreate) -> anyhow::Result<String>;
    async fn set_job_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<bool>;
    async fn run_job_now(&self, id: &str) -> anyhow::Result<String>;

    // --- projects (projects.read / projects.write) ---
    async fn list_projects(&self) -> anyhow::Result<Vec<AppProjectSummary>>;
    async fn create_chat_session(&self, title: Option<String>) -> anyhow::Result<String>;
    async fn attach_project(&self, session_pk: &str, project_id: &str) -> anyhow::Result<()>;
}

/// The app-control tool names — added to the sub-agent blocklist and never
/// advertised to delegated children (spec §9.1).
pub const APP_TOOLS: &[&str] = &["app_jobs", "app_projects", "clarify"];

/// Channel bundle for tools whose EXECUTION is a user interaction
/// (`exitplanmode`, `askuserquestion`): they emit their own
/// `ApprovalRequested` and block on the reply, reusing the approval pipeline.
pub struct Interaction {
    pub approvals: Arc<crate::approval::ApprovalHub>,
    pub events: tokio::sync::broadcast::Sender<crate::domain::CoreEvent>,
    pub run_id: String,
    pub requesting_agent_id: String,
    pub requesting_agent_name: String,
    /// The session's live permission mode (shared with `RunnerDeps`).
    pub perm_mode: Arc<std::sync::Mutex<crate::domain::PermMode>>,
    pub project_id: Option<String>,
}

impl Interaction {
    /// Park an interaction request and await the user's reply. `None` when the
    /// turn was cancelled or the session dropped the sender.
    #[allow(clippy::too_many_arguments)]
    pub async fn request(
        &self,
        session_pk: &str,
        request_id: &str,
        tool: &str,
        summary: &str,
        approval_kind: crate::domain::ApprovalKind,
        input: serde_json::Value,
        cancel: &CancellationToken,
    ) -> Option<crate::domain::ApprovalResponse> {
        let key = ApprovalKey::new(&self.run_id, request_id);
        let rx = self.approvals.register_for_session(session_pk, key.clone());
        let _ = self
            .events
            .send(crate::domain::CoreEvent::ApprovalRequested {
                session_pk: session_pk.to_string(),
                run_id: self.run_id.clone(),
                requesting_agent_id: self.requesting_agent_id.clone(),
                requesting_agent_name: self.requesting_agent_name.clone(),
                request_id: request_id.to_string(),
                tool: tool.to_string(),
                summary: summary.to_string(),
                approval_kind,
                input,
                // Plan/Question prompts are core-agent interactions, never a
                // plugin's MCP tool call — `principal` is only ever resolved
                // for `mcp__<server>__<tool>` calls via `permission::evaluate`.
                principal: None,
            });
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                self.approvals.resolve_bool(&key, false);
                None
            }
            res = rx => res.ok(),
        }
    }
}

/// Everything a tool needs to run one call.
pub struct ToolCtx {
    pub session_pk: String,
    /// The durable agent run owning this call; approvals and child runs are
    /// always scoped to this identity.
    pub run_id: String,
    /// The session worktree — the sandbox jail root.
    pub work_dir: PathBuf,
    /// The session's attachment folder (`…/.harness-attachments/{session_pk}`)
    /// — a SECOND read root beside the worktree jail, so the model can open
    /// files the user attached. `None` in bare test contexts.
    pub attachments_dir: Option<PathBuf>,
    /// Plugin-bundled skill directories (see
    /// `crate::plugins::PluginHost::enabled_skill_dirs`), consulted by the
    /// `skill` tool alongside `work_dir`'s own skill dirs.
    pub extra_skill_dirs: Vec<PathBuf>,
    pub store: Arc<Store>,
    pub cancel: CancellationToken,
    pub caps: OutputCaps,
    /// Sub-agent spawner for the `task` tool; `None` disables spawning.
    pub spawn: Option<Arc<dyn SubagentSpawner>>,
    /// Complete profile spawner for `delegate_agent`; distinct from `task`.
    pub main_agent_spawn: Option<Arc<dyn MainAgentSpawner>>,
    /// Persistent memory for the `memory` tool; `None` for sub-agents.
    pub memory: Option<Arc<crate::harness::native::memory::MemoryStore>>,
    /// Stack of worktree snapshot SHAs for the `revert` tool (most recent last).
    pub snapshots: Arc<tokio::sync::Mutex<Vec<String>>>,
    /// This call's tool_use id — doubles as the approval request_id for
    /// interaction tools. Empty only in bare test contexts.
    pub tool_call_id: String,
    /// Present when the session has interactive surfaces; `None` disables
    /// `exitplanmode`/`askuserquestion` (they return a tool error).
    pub interaction: Option<Arc<Interaction>>,
    /// Curated app-control facade (spec §9.1). `None` disables the `app_*`
    /// tools (sub-agents, workers, review forks, bare tests) — they return a
    /// "not available" error, mirroring `interaction`/`spawn`/`memory`.
    pub app: Option<Arc<dyn AppControl>>,
    /// Which actor is driving this tool call (Phase 4 §7). Defaults to
    /// `User` for interactive turns; the background review fork
    /// (`WriteOrigin::BackgroundReview`) and sub-agent turns
    /// (`WriteOrigin::Agent`) set it explicitly at their own `ToolCtx` build
    /// sites. Consulted by `skill_manage` and the app-control negative-space
    /// guard (Phase 6) to gate autonomous writes more strictly than
    /// human-driven ones.
    pub write_origin: crate::domain::WriteOrigin,
    /// Skill names viewed so far this tool-call turn — the `skill` tool
    /// records into this set so a same-turn `skill_manage` USE can tell
    /// "viewed-then-used" apart from "used blind", without a DB round trip.
    pub viewed_skills: Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
}

/// The result of a tool call.
pub struct ToolOutput {
    /// Text replayed to the model as the `tool_result` content (already
    /// truncated to caps by the tool).
    pub for_model: String,
    /// Optional content blocks (e.g. `{type:"image",…}`) PREPENDED to the
    /// tool_result content before `for_model`'s text block. `None` for
    /// text-only results (every tool but image reads).
    pub model_blocks: Option<Vec<Value>>,
    /// Optional extra fields merged into the persisted `tool_call` payload for
    /// the UI (e.g. a status summary). `None` for most tools.
    pub display: Option<Value>,
    pub is_error: bool,
    pub structured_error: Option<ToolError>,
}

impl ToolOutput {
    pub fn ok(text: impl Into<String>) -> Self {
        ToolOutput {
            for_model: text.into(),
            model_blocks: None,
            display: None,
            is_error: false,
            structured_error: None,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        let text = text.into();
        ToolOutput {
            for_model: text.clone(),
            model_blocks: None,
            display: None,
            is_error: true,
            structured_error: Some(ToolError::internal("tool_failed", text)),
        }
    }

    pub fn from_error(error: ToolError) -> Self {
        ToolOutput {
            for_model: error.public_message(),
            model_blocks: None,
            display: None,
            is_error: true,
            structured_error: Some(error),
        }
    }
}

/// How a tool call is gated: a permission `key` (matched against `PermMode` /
/// project policy) and a human `summary` for the approval prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSpec {
    pub key: String,
    pub summary: String,
    /// Which plugin this call's tool belongs to, if any — attribution only,
    /// carried through to the approval prompt (see
    /// [`crate::domain::Principal`]). Built-in tools never set this; only
    /// [`mcp::McpTool`] resolves one, from the mcp-server→plugin binding.
    pub principal: Option<crate::domain::Principal>,
}

impl PermissionSpec {
    pub fn new(key: impl Into<String>, summary: impl Into<String>) -> Self {
        PermissionSpec {
            key: key.into(),
            summary: summary.into(),
            principal: None,
        }
    }

    /// Attach a resolved plugin principal (or clear it, given `None`).
    pub fn with_principal(mut self, principal: Option<crate::domain::Principal>) -> Self {
        self.principal = principal;
        self
    }
}

/// A built-in tool the native runtime can call.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool id as exposed to the model (e.g. `"bash"`). Built-ins return a
    /// static string; MCP tools return a dynamic `mcp__server__tool` name.
    fn name(&self) -> &str;
    /// Description text included in the tool definition sent to the model.
    fn description(&self) -> &str;
    /// Hand-written JSON schema for the tool input.
    fn input_schema(&self) -> Value;
    /// `tool_kind` column for the Cockpit UI: read|edit|search|execute|fetch|other.
    fn kind(&self) -> &'static str;
    /// Serializable contract metadata. Runtime resolvers remain on [`Tool`].
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor::conservative(
            self.name(),
            self.description(),
            self.input_schema(),
            self.kind(),
        )
    }
    /// Normalize arguments using only safe path-resolution inputs.
    fn normalize_input(
        &self,
        _ctx: &ToolInputCtx<'_>,
        input: Value,
    ) -> Result<NormalizedInput, ToolError> {
        Ok(NormalizedInput::unchanged(input))
    }
    /// Resolve lightweight metadata before permission and execution.
    async fn preflight(
        &self,
        _ctx: &ToolInputCtx<'_>,
        _input: &Value,
    ) -> Result<PreflightMeta, ToolError> {
        Ok(PreflightMeta::default())
    }
    /// Check whether the tool's external dependency is currently usable.
    async fn probe_availability(&self) -> AvailabilityProbe {
        AvailabilityProbe::Available
    }
    /// Permission gate for a specific call.
    fn permission(&self, input: &Value) -> PermissionSpec;
    /// Execute the call.
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput>;

    /// The Anthropic tool definition object for this tool.
    fn definition(&self) -> Value {
        serde_json::json!({
            "name": self.name(),
            "description": self.description(),
            "input_schema": self.input_schema(),
        })
    }
}

pub struct RegisteredTool {
    pub tool: Arc<dyn Tool>,
    pub descriptor: ToolDescriptor,
    pub canonical_schema: Value,
    pub canonical_validator: Option<Arc<jsonschema::Validator>>,
    pub v2_schema_eligible: bool,
    pub v2_schema_error: Option<ToolError>,
    pub openai_strict_schema: Option<Value>,
    pub strict_wire_eligible: bool,
    pub strict_wire_error: Option<ToolError>,
    pub contract_hash: String,
}

impl std::fmt::Debug for RegisteredTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredTool")
            .field("name", &self.descriptor.canonical_name)
            .field("v2_schema_eligible", &self.v2_schema_eligible)
            .field("strict_wire_eligible", &self.strict_wire_eligible)
            .field("contract_hash", &self.contract_hash)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct AvailableTool {
    pub registered: Arc<RegisteredTool>,
    pub stale: bool,
}

#[derive(Debug, Clone)]
struct AvailabilityCacheEntry {
    checked_at: tokio::time::Instant,
    probe: AvailabilityProbe,
    last_good_at: Option<tokio::time::Instant>,
}

pub const AVAILABILITY_TTL: Duration = Duration::from_secs(30);
pub const AVAILABILITY_LAST_GOOD_GRACE: Duration = Duration::from_secs(60);

static NEXT_REGISTRY_GENERATION: AtomicU64 = AtomicU64::new(1);

/// The set of tools available to a session, keyed by name. Built-ins plus any
/// per-session MCP tools.
pub struct ToolRegistry {
    legacy_tools: BTreeMap<String, Arc<RegisteredTool>>,
    canonical_tools: BTreeMap<String, Arc<RegisteredTool>>,
    legacy_to_canonical: BTreeMap<String, String>,
    generation: u64,
    availability: BTreeMap<String, Arc<tokio::sync::Mutex<Option<AvailabilityCacheEntry>>>>,
}

impl ToolRegistry {
    /// All built-in tools.
    pub fn builtin() -> Self {
        Self::from_complete_list(Self::builtin_list())
    }

    /// The built-ins plus a set of extra (e.g. MCP) tools.
    pub fn with_extra(extra: Vec<Arc<dyn Tool>>) -> Self {
        let mut list = Self::builtin_list();
        list.extend(extra);
        Self::from_complete_list(list)
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.legacy_tools
            .get(name)
            .map(|registered| registered.tool.clone())
    }

    pub fn registered(&self, name: &str) -> Option<Arc<RegisteredTool>> {
        self.canonical_tools.get(name).cloned()
    }

    /// The immutable canonical contracts captured when this registry was built.
    pub fn canonical_snapshot(&self) -> impl Iterator<Item = &Arc<RegisteredTool>> {
        self.canonical_tools.values()
    }

    /// The immutable effective legacy aliases captured with [`Self::get`]
    /// last-wins semantics when this registry was built.
    pub fn legacy_to_canonical(&self) -> &BTreeMap<String, String> {
        &self.legacy_to_canonical
    }

    /// The Anthropic `tools` array for a provider request.
    pub fn definitions(&self) -> Vec<Value> {
        self.legacy_tools
            .values()
            .filter(|registered| !registered.descriptor.v2_only)
            .map(|registered| registered.tool.definition())
            .collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.legacy_tools.keys().cloned().collect()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn v2_definition(
        &self,
        name: &str,
        capabilities: &ToolCapabilityProfile,
    ) -> Result<Value, ToolError> {
        if capabilities.interaction_mode == ToolInteractionMode::CodeOrchestrator {
            return Err(ToolError::precondition(
                "direct_tool_surface_unavailable",
                "Direct function tools are unavailable for this capability profile",
            ));
        }
        let registered = self.canonical_tools.get(name).ok_or_else(|| {
            ToolError::precondition("tool_not_found", "Tool is not present in this registry")
        })?;
        if !registered.v2_schema_eligible {
            return Err(registered.v2_schema_error.clone().unwrap_or_else(|| {
                ToolError::precondition("tool_not_v2_eligible", "Tool is not eligible for V2")
            }));
        }

        let (schema, strict) =
            if capabilities.supports_strict_function_schema && registered.strict_wire_eligible {
                (
                    registered
                        .openai_strict_schema
                        .clone()
                        .expect("eligible strict schema is compiled at registry construction"),
                    true,
                )
            } else {
                (registered.canonical_schema.clone(), false)
            };

        let mut definition = serde_json::json!({
            "name": registered.descriptor.canonical_name,
            "description": registered.descriptor.description,
            "input_schema": schema,
            "strict": strict,
        });
        if capabilities.supports_tool_output_schema {
            if let Some(output_schema) = &registered.descriptor.output_schema {
                definition["output_schema"] = output_schema.clone();
            }
        }
        Ok(definition)
    }

    pub async fn available(&self, name: &str) -> Result<Option<AvailableTool>, ToolError> {
        let Some(registered) = self.canonical_tools.get(name).cloned() else {
            return Ok(None);
        };
        let cache = self
            .availability
            .get(name)
            .expect("availability entries are built from the immutable tool snapshot");
        let mut cache = cache.lock().await;
        let evaluation_time = tokio::time::Instant::now();
        if let Some(entry) = cache.as_ref() {
            if evaluation_time.duration_since(entry.checked_at) < AVAILABILITY_TTL {
                return availability_from_entry(registered, entry, evaluation_time).map(Some);
            }
        }

        let previous_last_good = cache.as_ref().and_then(|entry| entry.last_good_at);
        let probe = registered.tool.probe_availability().await;
        let completed_at = tokio::time::Instant::now();
        let last_good_at = if matches!(probe, AvailabilityProbe::Available) {
            Some(completed_at)
        } else {
            previous_last_good
        };
        let entry = AvailabilityCacheEntry {
            checked_at: completed_at,
            probe,
            last_good_at,
        };
        let result = availability_from_entry(registered, &entry, completed_at).map(Some);
        *cache = Some(entry);
        result
    }

    fn builtin_list() -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(read::Read),
            Arc::new(ls::Ls),
            Arc::new(write::Write),
            Arc::new(edit::Edit),
            Arc::new(glob::Glob),
            Arc::new(grep::Grep),
            Arc::new(bash::Bash),
            Arc::new(todo::TodoWrite),
            Arc::new(todo::TodoRead),
            Arc::new(webfetch::WebFetch),
            Arc::new(websearch::WebSearch),
            Arc::new(skill::SkillTool),
            Arc::new(skill_manage::SkillManage),
            Arc::new(memory::MemoryTool),
            Arc::new(memory::MemoryAdd),
            Arc::new(memory::MemoryReplace),
            Arc::new(memory::MemoryRemove),
            Arc::new(memory::MemoryBatch),
            Arc::new(revert::Revert),
            Arc::new(lsp::Lsp),
            Arc::new(task::Task),
            Arc::new(delegate::DelegateAgent),
            Arc::new(session_search::SessionSearch),
            Arc::new(plan::ExitPlanMode),
            Arc::new(question::AskUserQuestion),
            Arc::new(app_jobs::AppJobs),
            Arc::new(app_projects::AppProjects),
            Arc::new(clarify::Clarify),
        ]
    }

    fn from_complete_list(list: Vec<Arc<dyn Tool>>) -> Self {
        let mut legacy_tools = BTreeMap::new();
        let mut canonical_tools = BTreeMap::new();
        let mut legacy_to_canonical = BTreeMap::new();
        for tool in list {
            let registered = Arc::new(compile_registered_tool(tool));
            let legacy_name = registered.tool.name().to_string();
            let canonical_name = registered.descriptor.canonical_name.clone();
            legacy_to_canonical.insert(legacy_name.clone(), canonical_name.clone());
            legacy_tools.insert(legacy_name, registered.clone());
            canonical_tools.insert(canonical_name, registered);
        }
        let availability = canonical_tools
            .keys()
            .map(|name| (name.clone(), Arc::new(tokio::sync::Mutex::new(None))))
            .collect();
        Self {
            legacy_tools,
            canonical_tools,
            legacy_to_canonical,
            generation: next_registry_generation(),
            availability,
        }
    }
}

fn next_registry_generation() -> u64 {
    NEXT_REGISTRY_GENERATION
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |generation| {
            generation.checked_add(1)
        })
        .expect("tool registry generation exhausted")
}

fn availability_from_entry(
    registered: Arc<RegisteredTool>,
    entry: &AvailabilityCacheEntry,
    now: tokio::time::Instant,
) -> Result<AvailableTool, ToolError> {
    match &entry.probe {
        AvailabilityProbe::Available => Ok(AvailableTool {
            registered,
            stale: false,
        }),
        AvailabilityProbe::Unavailable { transient, .. }
            if *transient
                && entry.last_good_at.is_some_and(|last_good| {
                    now.duration_since(last_good) <= AVAILABILITY_LAST_GOOD_GRACE
                }) =>
        {
            Ok(AvailableTool {
                registered,
                stale: true,
            })
        }
        AvailabilityProbe::Unavailable { code, transient } => Err(if *transient {
            ToolError::transient(code, "Tool is temporarily unavailable")
        } else {
            ToolError::precondition(code, "Tool is unavailable")
        }),
    }
}

fn compile_registered_tool(tool: Arc<dyn Tool>) -> RegisteredTool {
    let descriptor = tool.descriptor();
    let description_bytes = descriptor.description.len();
    let serialized_input_schema = serde_json::to_vec(&descriptor.input_schema).unwrap_or_default();
    let schema_bytes = serialized_input_schema.len();
    let size_error = if description_bytes > MAX_TOOL_DESCRIPTION_BYTES {
        Some(contract_size_error(
            "MAX_TOOL_DESCRIPTION_BYTES",
            description_bytes,
            MAX_TOOL_DESCRIPTION_BYTES,
        ))
    } else if schema_bytes > MAX_TOOL_SCHEMA_BYTES {
        Some(contract_size_error(
            "MAX_TOOL_SCHEMA_BYTES",
            schema_bytes,
            MAX_TOOL_SCHEMA_BYTES,
        ))
    } else {
        None
    };

    let canonical_schema = if size_error.is_none() {
        compile_canonical_schema(descriptor.input_schema.clone())
    } else {
        Value::Null
    };

    let canonical_validator = if size_error.is_none() {
        jsonschema::validator_for(&canonical_schema)
            .ok()
            .map(Arc::new)
    } else {
        None
    };
    let invalid_error = (size_error.is_none() && canonical_validator.is_none()).then(|| {
        ToolError::precondition("invalid_tool_schema", "Tool input schema is invalid")
            .with_details(serde_json::json!({"reason": "schema_compilation_failed"}))
    });
    let open_error = (size_error.is_none()
        && invalid_error.is_none()
        && explicit_open_object_schema(&descriptor.input_schema))
    .then(|| {
        ToolError::precondition(
            "unsupported_open_object_schema",
            "Tool input schema contains an explicitly open object shape",
        )
        .with_details(serde_json::json!({"reason": "explicit_additional_properties"}))
    });
    let v1_only_error = descriptor
        .v1_only
        .then(|| ToolError::precondition("tool_not_v2_eligible", "Tool is restricted to V1"));
    let v2_schema_error = size_error
        .or(invalid_error)
        .or(open_error)
        .or(v1_only_error);
    let v2_schema_eligible = v2_schema_error.is_none();

    let strict_result = v2_schema_eligible
        .then(|| compile_openai_strict_schema(&canonical_schema))
        .transpose();
    let (openai_strict_schema, strict_wire_error) = match strict_result {
        Ok(schema) => (schema, None),
        Err(error) => (
            None,
            Some(ToolError::precondition(error.code, error.message)),
        ),
    };
    let strict_wire_eligible = openai_strict_schema.is_some();

    let contract_hash = contract_hash(
        &descriptor,
        &serialized_input_schema,
        &canonical_schema,
        &openai_strict_schema,
    );

    RegisteredTool {
        tool,
        descriptor,
        canonical_schema,
        canonical_validator,
        v2_schema_eligible,
        v2_schema_error,
        openai_strict_schema,
        strict_wire_eligible,
        strict_wire_error,
        contract_hash,
    }
}

fn contract_hash(
    descriptor: &ToolDescriptor,
    serialized_input_schema: &[u8],
    canonical_schema: &Value,
    openai_strict_schema: &Option<Value>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ryuzi-tool-contract-v2\0");
    hash_part(&mut hasher, &descriptor.canonical_name);
    hash_part(&mut hasher, &descriptor.description);
    hasher.update((serialized_input_schema.len() as u64).to_le_bytes());
    hasher.update(serialized_input_schema);
    hash_part(&mut hasher, &descriptor.output_schema);
    hash_part(&mut hasher, &descriptor.kind);
    hash_part(&mut hasher, &descriptor.effect);
    hash_part(&mut hasher, &descriptor.idempotent);
    hash_part(&mut hasher, &descriptor.interactive);
    hash_part(&mut hasher, &descriptor.sequential_barrier);
    hash_part(&mut hasher, &descriptor.resource_scope);
    hash_part(&mut hasher, &descriptor.result_limit_bytes);
    hash_part(&mut hasher, &descriptor.facade_priority);
    hash_part(&mut hasher, &descriptor.policy_aliases);
    hash_part(&mut hasher, &descriptor.v2_only);
    hash_part(&mut hasher, &descriptor.v1_only);
    hash_part(&mut hasher, &descriptor.allow_lossless_coercions);
    hash_part(&mut hasher, canonical_schema);
    hash_part(&mut hasher, openai_strict_schema);
    format!("{:x}", hasher.finalize())
}

fn hash_part(hasher: &mut Sha256, value: &impl Serialize) {
    struct HashWriter<'a>(&'a mut Sha256);

    impl std::io::Write for HashWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut writer = HashWriter(hasher);
    serde_json::to_writer(&mut writer, value).expect("tool contract fields are serializable");
    writer.0.update(b"\0");
}

fn contract_size_error(limit_name: &str, actual_bytes: usize, max_bytes: usize) -> ToolError {
    ToolError::precondition(
        "tool_contract_too_large",
        "Tool contract exceeds a safety limit",
    )
    .with_details(serde_json::json!({
        "limit": limit_name,
        "actual_bytes": actual_bytes,
        "max_bytes": max_bytes,
    }))
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

/// Resolve `rel` against the session worktree, rejecting any escape.
pub fn jail(work_dir: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    sandbox(work_dir, Path::new(rel))
}

/// Resolve `path` relative to `work_dir` and verify it stays inside `work_dir`.
///
/// Rules:
/// - If `path` is relative it is joined onto `work_dir`.
/// - If `path` is absolute it must already start with `work_dir`.
/// - After joining, `..` components are resolved lexically by normalizing the
///   combined path, then the lowest existing ancestor is canonicalized. This
///   blocks traversal escapes while allowing the file (or its parent dirs) to
///   not exist yet (e.g. for write targets).
///
/// Returns the resolved absolute path on success, or an error if the path
/// escapes the worktree.
fn sandbox(work_dir: &Path, path: &Path) -> anyhow::Result<PathBuf> {
    // Canonicalize work_dir so we compare against the real on-disk root and so
    // a symlinked work_dir doesn't cause false rejections on relative paths.
    let canonical_root = work_dir.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "sandbox: cannot canonicalize work_dir {}: {e}",
            work_dir.display()
        )
    })?;

    // Construct the candidate (absolute) path, resolving `..` lexically.
    // Use the *canonicalized* root as the base for relative joins so that any
    // symlink in work_dir is resolved before we concatenate the user path.
    let raw = if path.is_absolute() {
        path.to_path_buf()
    } else {
        canonical_root.join(path)
    };

    // Lexically normalize: walk components and collapse `..` without I/O.
    // This catches `..` escapes before any canonicalize call.
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in raw.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            other => parts.push(other.as_os_str().to_owned()),
        }
    }
    let normalized: PathBuf = parts.iter().collect();

    // Resolve the deepest existing ancestor to resolve any symlinks in the
    // directory chain and verify it remains under the root. This also permits
    // a caller to use a non-canonical alias of an in-tree absolute path (such
    // as macOS's `/var` alias for `/private/var`).
    let mut ancestor = normalized.as_path();
    loop {
        if ancestor.exists() {
            let canonical_ancestor = ancestor.canonicalize().map_err(|e| {
                anyhow::anyhow!("sandbox: cannot canonicalize {}: {e}", ancestor.display())
            })?;
            // Verify the canonicalized ancestor is still under the root.
            if !canonical_ancestor.starts_with(&canonical_root) {
                anyhow::bail!(
                    "sandbox: path {} escapes the worktree {} (symlink)",
                    path.display(),
                    canonical_root.display()
                );
            }
            // Reconstruct: canonical_ancestor + the suffix that didn't exist.
            // NOTE: PathBuf::join("") appends a trailing slash which causes
            // "Not a directory" on stat, so guard the empty-suffix case.
            let suffix = normalized
                .strip_prefix(ancestor)
                .unwrap_or(std::path::Path::new(""));
            if suffix == std::path::Path::new("") {
                return Ok(canonical_ancestor);
            }
            return Ok(canonical_ancestor.join(suffix));
        }
        match ancestor.parent() {
            Some(p) => ancestor = p,
            None => anyhow::bail!("sandbox: cannot resolve any ancestor of {}", path.display()),
        }
    }
}

/// Truncate model-visible output to the caps, preserving the head and tail and
/// marking what was dropped. Under caps, returns the text unchanged.
pub fn truncate(text: &str, caps: &OutputCaps) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let over_lines = lines.len() > caps.max_lines;
    let over_bytes = text.len() > caps.max_bytes;
    if !over_lines && !over_bytes {
        return text.to_string();
    }
    // Keep the first and last halves of the line budget.
    let keep = caps.max_lines.max(2);
    let head_n = keep / 2;
    let tail_n = keep - head_n;
    let head = lines.iter().take(head_n).cloned().collect::<Vec<_>>();
    let tail = lines
        .iter()
        .skip(lines.len().saturating_sub(tail_n))
        .cloned()
        .collect::<Vec<_>>();
    let dropped = lines.len().saturating_sub(head.len() + tail.len());
    let mut out = String::new();
    out.push_str(&head.join("\n"));
    out.push_str(&format!(
        "\n\n… [truncated {dropped} lines; output exceeded {} lines / {} bytes] …\n\n",
        caps.max_lines, caps.max_bytes
    ));
    out.push_str(&tail.join("\n"));
    // Final byte guard: if head+tail themselves blow the byte cap, hard-cut.
    if out.len() > caps.max_bytes * 2 {
        out.truncate(caps.max_bytes);
        out.push_str("\n… [hard byte cap] …");
    }
    out
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    /// A `ToolCtx` rooted at `dir` with a fresh in-memory store and an
    /// unset cancellation token.
    pub async fn ctx_at(dir: &Path) -> ToolCtx {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());
        ToolCtx {
            session_pk: "test-session".into(),
            run_id: "test-run".into(),
            work_dir: dir.to_path_buf(),
            attachments_dir: None,
            extra_skill_dirs: vec![],
            store,
            cancel: CancellationToken::new(),
            caps: OutputCaps::default(),
            spawn: None,
            main_agent_spawn: None,
            memory: None,
            snapshots: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            tool_call_id: "test-call".into(),
            interaction: None,
            app: None,
            write_origin: crate::domain::WriteOrigin::User,
            viewed_skills: Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new())),
        }
    }

    /// `ctx_at` + a live Interaction pinned to `mode`. Returns the hub/events
    /// so tests can script the user's reply.
    pub async fn ctx_with_interaction(
        dir: &Path,
        mode: crate::domain::PermMode,
    ) -> (
        ToolCtx,
        Arc<crate::approval::ApprovalHub>,
        tokio::sync::broadcast::Receiver<crate::domain::CoreEvent>,
        Arc<std::sync::Mutex<crate::domain::PermMode>>,
    ) {
        let mut ctx = ctx_at(dir).await;
        let hub = Arc::new(crate::approval::ApprovalHub::new());
        let (tx, rx) = tokio::sync::broadcast::channel(8);
        let perm = Arc::new(std::sync::Mutex::new(mode));
        ctx.interaction = Some(Arc::new(Interaction {
            approvals: hub.clone(),
            events: tx,
            run_id: ctx.run_id.clone(),
            requesting_agent_id: "test-agent".into(),
            requesting_agent_name: "Test Agent".into(),
            perm_mode: perm.clone(),
            project_id: None,
        }));
        (ctx, hub, rx, perm)
    }

    /// A recording fake `AppControl` for tool unit tests.
    #[derive(Default)]
    pub struct FakeAppControl {
        pub created: std::sync::Mutex<Vec<AppJobCreate>>,
    }

    #[async_trait]
    impl AppControl for FakeAppControl {
        fn origin(&self) -> crate::domain::WriteOrigin {
            crate::domain::WriteOrigin::Agent
        }
        async fn list_jobs(&self) -> anyhow::Result<Vec<AppJobSummary>> {
            Ok(vec![AppJobSummary {
                id: "job-1".into(),
                name: "nightly".into(),
                cron: "0 9 * * *".into(),
                enabled: true,
            }])
        }
        async fn create_job(&self, spec: AppJobCreate) -> anyhow::Result<String> {
            self.created.lock().unwrap().push(spec);
            Ok("job-new".into())
        }
        async fn set_job_enabled(&self, _id: &str, _enabled: bool) -> anyhow::Result<bool> {
            Ok(true)
        }
        async fn run_job_now(&self, _id: &str) -> anyhow::Result<String> {
            Ok("run-1".into())
        }
        async fn list_projects(&self) -> anyhow::Result<Vec<AppProjectSummary>> {
            Ok(vec![AppProjectSummary {
                id: "p1".into(),
                name: "Ryuzi".into(),
            }])
        }
        async fn create_chat_session(&self, _title: Option<String>) -> anyhow::Result<String> {
            Ok("chat-1".into())
        }
        async fn attach_project(&self, _session_pk: &str, _project_id: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::native::capabilities::{
        CapabilitySource, ToolCapabilityProfile, ToolInteractionMode, WireProtocol,
    };
    use crate::harness::native::tool_contract::{
        canonical_compilation_count, AvailabilityProbe, ToolError, ToolInputCtx,
        MAX_TOOL_DESCRIPTION_BYTES, MAX_TOOL_SCHEMA_BYTES,
    };
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ContractTool {
        name: &'static str,
        description: String,
        schema: Value,
        canonical_name: Option<&'static str>,
        first_probe_delay: Duration,
        probe_failure_transient: bool,
        probes: AtomicUsize,
    }

    struct BlockingProbeTool {
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    #[async_trait]
    impl Tool for BlockingProbeTool {
        fn name(&self) -> &str {
            "blocking_probe"
        }

        fn description(&self) -> &str {
            "blocking availability probe"
        }

        fn input_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }

        fn kind(&self) -> &'static str {
            "other"
        }

        fn permission(&self, _input: &Value) -> PermissionSpec {
            PermissionSpec::new(self.name(), "test")
        }

        async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> anyhow::Result<ToolOutput> {
            Ok(ToolOutput::ok("ok"))
        }

        async fn probe_availability(&self) -> AvailabilityProbe {
            self.started.notify_one();
            self.release.notified().await;
            AvailabilityProbe::Available
        }
    }

    impl ContractTool {
        fn new(name: &'static str, description: impl Into<String>, schema: Value) -> Self {
            Self {
                name,
                description: description.into(),
                schema,
                canonical_name: None,
                first_probe_delay: Duration::ZERO,
                probe_failure_transient: true,
                probes: AtomicUsize::new(0),
            }
        }

        fn with_canonical_name(mut self, canonical_name: &'static str) -> Self {
            self.canonical_name = Some(canonical_name);
            self
        }

        fn with_first_probe_delay(mut self, delay: Duration) -> Self {
            self.first_probe_delay = delay;
            self
        }

        fn with_hard_failure(mut self) -> Self {
            self.probe_failure_transient = false;
            self
        }

        fn transient_after_first_success(name: &'static str) -> Self {
            Self::new(
                name,
                "availability test",
                serde_json::json!({"type": "object"}),
            )
        }
    }

    #[async_trait]
    impl Tool for ContractTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            &self.description
        }

        fn input_schema(&self) -> Value {
            self.schema.clone()
        }

        fn kind(&self) -> &'static str {
            "other"
        }

        fn descriptor(&self) -> ToolDescriptor {
            let mut descriptor = ToolDescriptor::conservative(
                self.name(),
                self.description(),
                self.input_schema(),
                self.kind(),
            );
            if let Some(canonical_name) = self.canonical_name {
                descriptor.canonical_name = canonical_name.into();
            }
            descriptor
        }

        fn permission(&self, _input: &Value) -> PermissionSpec {
            PermissionSpec::new(self.name, "test")
        }

        async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> anyhow::Result<ToolOutput> {
            Ok(ToolOutput::ok("ok"))
        }

        async fn probe_availability(&self) -> AvailabilityProbe {
            let probe_index = self.probes.fetch_add(1, Ordering::SeqCst);
            if probe_index == 0 && !self.first_probe_delay.is_zero() {
                tokio::time::advance(self.first_probe_delay).await;
            }
            if probe_index == 0 {
                AvailabilityProbe::Available
            } else {
                AvailabilityProbe::Unavailable {
                    code: "temporarily_unavailable".into(),
                    transient: self.probe_failure_transient,
                }
            }
        }
    }

    fn strict_capabilities() -> ToolCapabilityProfile {
        ToolCapabilityProfile {
            interaction_mode: ToolInteractionMode::DirectFunctions,
            wire_protocol: WireProtocol::OpenAiResponses,
            supports_custom_freeform_tools: false,
            supports_parallel_tool_calls: true,
            supports_strict_function_schema: true,
            supports_tool_output_schema: true,
            schema_budget_tokens: 16_000,
            supports_prompt_cache: true,
            capability_source: CapabilitySource::TransportDefault,
            capability_schema_version:
                crate::harness::native::capabilities::CAPABILITY_SCHEMA_VERSION,
        }
    }

    #[test]
    fn jail_accepts_in_tree_and_rejects_escapes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.txt"), "x").unwrap();
        // In-tree relative path resolves under the root.
        let p = jail(root, "a.txt").unwrap();
        assert!(p.starts_with(root.canonicalize().unwrap()));
        // Traversal escape is rejected.
        assert!(jail(root, "../etc/passwd").is_err());
        // Absolute outside path is rejected.
        assert!(jail(root, "/etc/passwd").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_accepts_a_canonical_equivalent_absolute_path() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("root");
        let alias = parent.path().join("alias");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("notes.txt"), "x").unwrap();
        symlink(&root, &alias).unwrap();

        assert_eq!(
            sandbox(&root, &alias.join("notes.txt")).unwrap(),
            root.join("notes.txt").canonicalize().unwrap()
        );
    }

    #[test]
    fn sandbox_confines_to_work_dir_and_rejects_escapes() {
        let root = tempfile::tempdir().unwrap();
        // Canonicalize the root: on macOS tempdir() lives under /var -> /private/var,
        // and sandbox() canonicalizes work_dir, so the raw root.path() prefix wouldn't
        // match the returned canonicalized path.
        let root_path = root.path().canonicalize().unwrap();
        // an in-root relative path resolves under root:
        let ok = sandbox(&root_path, Path::new("sub/file.txt")).unwrap();
        assert!(ok.starts_with(&root_path));
        // escapes are rejected:
        assert!(
            sandbox(&root_path, Path::new("../../etc/passwd")).is_err(),
            "expected .. escape to be rejected"
        );
        assert!(
            sandbox(&root_path, Path::new("/etc/passwd")).is_err(),
            "expected absolute path outside root to be rejected"
        );
    }

    #[cfg(windows)]
    #[test]
    fn sandbox_accepts_an_in_tree_absolute_path() {
        let root = tempfile::tempdir().unwrap();
        let file_path = root.path().join("notes.txt");
        fs::write(&file_path, "hello\n").unwrap();

        assert_eq!(
            sandbox(root.path(), &file_path).unwrap(),
            file_path.canonicalize().unwrap()
        );
    }

    #[test]
    fn truncate_passes_through_small_output() {
        let caps = OutputCaps::default();
        assert_eq!(truncate("hello\nworld", &caps), "hello\nworld");
    }

    #[test]
    fn truncate_keeps_head_and_tail_and_marks_drop() {
        let caps = OutputCaps {
            max_lines: 4,
            max_bytes: 1_000_000,
        };
        let text = (1..=100)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let out = truncate(&text, &caps);
        assert!(out.starts_with("1\n2"));
        assert!(out.trim_end().ends_with("100"));
        assert!(out.contains("truncated"));
        assert!(out.contains("99\n100"));
    }

    #[tokio::test]
    async fn tool_ctx_carries_app_facade_and_write_origin() {
        use super::testutil::ctx_at;
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ctx_at(dir.path()).await;
        // Default (bare test) context: no facade, user origin.
        assert!(ctx.app.is_none());
        assert_eq!(ctx.write_origin, crate::domain::WriteOrigin::User);
        // A fake facade can be attached and called.
        let fake = std::sync::Arc::new(testutil::FakeAppControl::default());
        ctx.app = Some(fake.clone());
        ctx.write_origin = crate::domain::WriteOrigin::Agent;
        let id = ctx
            .app
            .as_ref()
            .unwrap()
            .create_job(AppJobCreate {
                name: "nightly".into(),
                schedule: "every day at 9am".into(),
                prompt: "summarize".into(),
                project_id: Some("p1".into()),
                model_override: None,
            })
            .await
            .unwrap();
        assert!(!id.is_empty());
        assert_eq!(fake.created.lock().unwrap().len(), 1);
    }

    #[test]
    fn registry_has_all_builtins_with_definitions() {
        let reg = ToolRegistry::builtin();
        for name in [
            "read",
            "ls",
            "write",
            "edit",
            "glob",
            "grep",
            "bash",
            "todowrite",
            "todoread",
            "webfetch",
            "websearch",
            "skill",
            "skill_manage",
            "memory",
            "revert",
            "lsp",
            "task",
            "delegate_agent",
            "session_search",
            "exitplanmode",
            "askuserquestion",
            "app_jobs",
            "app_projects",
            "clarify",
        ] {
            assert!(reg.get(name).is_some(), "missing tool {name}");
        }
        let defs = reg.definitions();
        assert_eq!(defs.len(), 24);
        assert!(defs.iter().all(|d| d.get("name").is_some()
            && d.get("description").is_some()
            && d.get("input_schema").is_some()));
    }

    #[test]
    fn registry_generations_are_monotonic() {
        let first = ToolRegistry::builtin().generation();
        let second = ToolRegistry::builtin().generation();
        assert!(second > first);
    }

    #[test]
    fn invalid_and_open_schemas_are_excluded_from_v2_without_changing_v1() {
        let invalid: Arc<dyn Tool> = Arc::new(ContractTool::new(
            "invalid_contract",
            "invalid",
            serde_json::json!({"type": 42}),
        ));
        let open: Arc<dyn Tool> = Arc::new(ContractTool::new(
            "open_contract",
            "open",
            serde_json::json!({
                "type": "object",
                "additionalProperties": true
            }),
        ));
        let schema_valued: Arc<dyn Tool> = Arc::new(ContractTool::new(
            "schema_valued_contract",
            "schema valued",
            serde_json::json!({
                "type": "object",
                "additionalProperties": {"type": "string"}
            }),
        ));
        let untyped_open: Arc<dyn Tool> = Arc::new(ContractTool::new(
            "untyped_open_contract",
            "untyped open",
            serde_json::json!({"additionalProperties": true}),
        ));
        let invalid_v1 = invalid.definition();
        let open_v1 = open.definition();
        let registry = ToolRegistry::with_extra(vec![invalid, open, schema_valued, untyped_open]);

        assert_eq!(
            registry
                .v2_definition("invalid_contract", &strict_capabilities())
                .unwrap_err()
                .code,
            "invalid_tool_schema"
        );
        assert_eq!(
            registry
                .v2_definition("open_contract", &strict_capabilities())
                .unwrap_err()
                .code,
            "unsupported_open_object_schema"
        );
        assert_eq!(
            registry
                .v2_definition("untyped_open_contract", &strict_capabilities())
                .unwrap_err()
                .code,
            "unsupported_open_object_schema"
        );
        assert_eq!(
            registry
                .v2_definition("schema_valued_contract", &strict_capabilities())
                .unwrap_err()
                .code,
            "unsupported_open_object_schema"
        );
        assert_eq!(
            registry.get("invalid_contract").unwrap().definition(),
            invalid_v1
        );
        assert_eq!(registry.get("open_contract").unwrap().definition(), open_v1);
    }

    #[test]
    fn strict_ineligible_v2_tools_fall_back_to_closed_canonical_schema() {
        let ambiguous: Arc<dyn Tool> = Arc::new(ContractTool::new(
            "ambiguous_null",
            "ambiguous",
            serde_json::json!({
                "type": "object",
                "properties": {"value": {"type": ["string", "null"]}}
            }),
        ));
        let one_of: Arc<dyn Tool> = Arc::new(ContractTool::new(
            "overlapping_one_of",
            "overlapping",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "value": {
                        "oneOf": [
                            {"type": "string"},
                            {"minLength": 1}
                        ]
                    }
                },
                "required": ["value"]
            }),
        ));
        let registry = ToolRegistry::with_extra(vec![ambiguous, one_of]);

        for name in ["ambiguous_null", "overlapping_one_of"] {
            let registered = registry.registered(name).unwrap();
            assert!(registered.v2_schema_eligible);
            assert!(!registered.strict_wire_eligible);

            let definition = registry
                .v2_definition(name, &strict_capabilities())
                .unwrap();
            assert_eq!(definition["name"], name);
            assert_eq!(definition["strict"], false);
            assert_eq!(definition["input_schema"], registered.canonical_schema);
            assert_eq!(definition["input_schema"]["additionalProperties"], false);
        }
    }

    #[test]
    fn strict_capable_profiles_can_advertise_builtin_strict_ineligible_tools() {
        let registry = ToolRegistry::builtin();

        for name in ["task", "delegate_agent"] {
            let definition = registry
                .v2_definition(name, &strict_capabilities())
                .unwrap();
            assert_eq!(definition["strict"], false);
            assert_eq!(definition["input_schema"]["additionalProperties"], false);
        }
    }

    #[test]
    fn oversized_contracts_report_only_bounded_size_details() {
        let description_marker = "secret-description-marker";
        let schema_marker = "secret-schema-marker";
        let oversized_description: Arc<dyn Tool> = Arc::new(ContractTool::new(
            "oversized_description",
            format!(
                "{description_marker}{}",
                "x".repeat(MAX_TOOL_DESCRIPTION_BYTES + 1)
            ),
            serde_json::json!({"type": "object"}),
        ));
        let oversized_schema: Arc<dyn Tool> = Arc::new(ContractTool::new(
            "oversized_schema",
            "oversized schema",
            serde_json::json!({
                "type": "object",
                "description": format!(
                    "{schema_marker}{}",
                    "x".repeat(MAX_TOOL_SCHEMA_BYTES + 1)
                )
            }),
        ));
        let registry = ToolRegistry::with_extra(vec![oversized_description, oversized_schema]);

        for name in ["oversized_description", "oversized_schema"] {
            let error = registry
                .v2_definition(name, &strict_capabilities())
                .unwrap_err();
            let serialized = serde_json::to_string(&error).unwrap();
            assert_eq!(error.code, "tool_contract_too_large");
            assert!(!serialized.contains(description_marker));
            assert!(!serialized.contains(schema_marker));
            assert!(serialized.len() < 512);
        }
    }

    #[test]
    fn oversized_schema_skips_recursive_contract_compilers() {
        let oversized: Arc<dyn Tool> = Arc::new(ContractTool::new(
            "oversized_compile_guard",
            "oversized schema",
            serde_json::json!({
                "type": "object",
                "description": "x".repeat(MAX_TOOL_SCHEMA_BYTES + 1)
            }),
        ));
        let before = canonical_compilation_count();

        let registered = compile_registered_tool(oversized);

        assert_eq!(canonical_compilation_count(), before);
        assert_eq!(registered.canonical_schema, Value::Null);
        assert_eq!(
            registered.v2_schema_error.unwrap().code,
            "tool_contract_too_large"
        );
        assert_eq!(registered.contract_hash.len(), 64);
    }

    #[test]
    fn contract_hash_is_stable_and_covers_descriptor_changes() {
        let first = compile_registered_tool(Arc::new(ContractTool::new(
            "stable_hash",
            "same description",
            serde_json::json!({"type": "object", "properties": {"id": {"type": "string"}}}),
        )));
        let second = compile_registered_tool(Arc::new(ContractTool::new(
            "stable_hash",
            "same description",
            serde_json::json!({"type": "object", "properties": {"id": {"type": "string"}}}),
        )));
        let changed = compile_registered_tool(Arc::new(ContractTool::new(
            "stable_hash",
            "changed description",
            serde_json::json!({"type": "object", "properties": {"id": {"type": "string"}}}),
        )));

        assert_eq!(first.contract_hash, second.contract_hash);
        assert_ne!(first.contract_hash, changed.contract_hash);
    }

    #[tokio::test]
    async fn descriptor_canonical_name_controls_snapshot_dedup_lookup_and_cache() {
        let first = Arc::new(
            ContractTool::new(
                "first_alias",
                "first",
                serde_json::json!({"type": "object"}),
            )
            .with_canonical_name("canonical_contract"),
        );
        let second = Arc::new(
            ContractTool::new(
                "second_alias",
                "second",
                serde_json::json!({"type": "object"}),
            )
            .with_canonical_name("canonical_contract"),
        );
        let registry = ToolRegistry::with_extra(vec![first.clone(), second.clone()]);

        assert_eq!(registry.get("first_alias").unwrap().name(), "first_alias");
        assert_eq!(registry.get("second_alias").unwrap().name(), "second_alias");
        assert!(registry.get("canonical_contract").is_none());
        assert_eq!(
            registry
                .registered("canonical_contract")
                .unwrap()
                .descriptor
                .description,
            "second"
        );
        assert_eq!(
            registry
                .v2_definition("canonical_contract", &strict_capabilities())
                .unwrap()["name"],
            "canonical_contract"
        );
        assert!(registry
            .available("canonical_contract")
            .await
            .unwrap()
            .is_some());
        assert_eq!(first.probes.load(Ordering::SeqCst), 0);
        assert_eq!(second.probes.load(Ordering::SeqCst), 1);
        assert!(registry.available("second_alias").await.unwrap().is_none());

        let v1_names = registry
            .definitions()
            .into_iter()
            .filter_map(|definition| definition["name"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        assert!(v1_names.iter().any(|name| name == "first_alias"));
        assert!(v1_names.iter().any(|name| name == "second_alias"));
        for name in v1_names {
            assert!(
                registry.get(&name).is_some(),
                "V1 definition {name} must resolve through get"
            );
        }
    }

    #[test]
    fn legacy_and_canonical_collisions_each_preserve_last_wins() {
        let first: Arc<dyn Tool> = Arc::new(
            ContractTool::new(
                "shared_alias",
                "legacy first",
                serde_json::json!({"type": "object"}),
            )
            .with_canonical_name("first_canonical"),
        );
        let second: Arc<dyn Tool> = Arc::new(
            ContractTool::new(
                "shared_alias",
                "legacy second",
                serde_json::json!({"type": "object"}),
            )
            .with_canonical_name("second_canonical"),
        );
        let canonical_first: Arc<dyn Tool> = Arc::new(
            ContractTool::new(
                "first_canonical_alias",
                "canonical first",
                serde_json::json!({"type": "object"}),
            )
            .with_canonical_name("shared_canonical"),
        );
        let canonical_second: Arc<dyn Tool> = Arc::new(
            ContractTool::new(
                "second_canonical_alias",
                "canonical second",
                serde_json::json!({"type": "object"}),
            )
            .with_canonical_name("shared_canonical"),
        );
        let registry =
            ToolRegistry::with_extra(vec![first, second, canonical_first, canonical_second]);

        let canonical_snapshot = registry
            .canonical_snapshot()
            .map(|registered| registered.descriptor.canonical_name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(canonical_snapshot.contains("first_canonical"));
        assert!(canonical_snapshot.contains("second_canonical"));
        assert_eq!(
            registry.legacy_to_canonical().get("shared_alias").unwrap(),
            "second_canonical"
        );

        assert_eq!(
            registry.get("shared_alias").unwrap().description(),
            "legacy second"
        );
        assert_eq!(
            registry
                .registered("first_canonical")
                .unwrap()
                .descriptor
                .description,
            "legacy first"
        );
        assert_eq!(
            registry
                .registered("second_canonical")
                .unwrap()
                .descriptor
                .description,
            "legacy second"
        );
        assert_eq!(
            registry
                .registered("shared_canonical")
                .unwrap()
                .descriptor
                .description,
            "canonical second"
        );

        let shared_alias_definitions = registry
            .definitions()
            .into_iter()
            .filter(|definition| definition["name"] == "shared_alias")
            .collect::<Vec<_>>();
        assert_eq!(shared_alias_definitions.len(), 1);
        assert_eq!(shared_alias_definitions[0]["description"], "legacy second");
    }

    #[test]
    fn default_descriptor_and_safe_input_hooks_are_conservative() {
        let tool = ContractTool::new(
            "mutating_test",
            "mutates",
            serde_json::json!({"type": "object"}),
        );
        let descriptor = tool.descriptor();
        let root = tempfile::tempdir().unwrap();
        let ctx = ToolInputCtx {
            work_dir: root.path(),
            attachments_dir: None,
            extra_skill_dirs: &[],
        };
        let input = serde_json::json!({"value": "unchanged"});
        let normalized = tool.normalize_input(&ctx, input.clone()).unwrap();

        assert!(!descriptor.idempotent);
        assert!(descriptor.sequential_barrier);
        assert_eq!(normalized.value, input);
        assert!(!normalized.normalized);
        assert!(normalized.metadata().is_empty());
    }

    #[tokio::test]
    async fn default_preflight_and_probe_are_safe_no_ops() {
        let tool = ContractTool::new(
            "safe_defaults",
            "safe defaults",
            serde_json::json!({"type": "object"}),
        );
        let root = tempfile::tempdir().unwrap();
        let ctx = ToolInputCtx {
            work_dir: root.path(),
            attachments_dir: None,
            extra_skill_dirs: &[],
        };

        assert_eq!(
            tool.preflight(&ctx, &serde_json::json!({})).await.unwrap(),
            Default::default()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn transient_probe_uses_last_good_only_inside_grace_window() {
        let tool = Arc::new(ContractTool::transient_after_first_success(
            "flaky_contract",
        ));
        let registry = ToolRegistry::with_extra(vec![tool.clone()]);

        let fresh = registry.available("flaky_contract").await.unwrap().unwrap();
        assert!(!fresh.stale);
        assert_eq!(tool.probes.load(Ordering::SeqCst), 1);

        tokio::time::advance(std::time::Duration::from_secs(29)).await;
        let cached = registry.available("flaky_contract").await.unwrap().unwrap();
        assert!(!cached.stale);
        assert_eq!(tool.probes.load(Ordering::SeqCst), 1);

        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        let stale = registry.available("flaky_contract").await.unwrap().unwrap();
        assert!(stale.stale);
        assert_eq!(tool.probes.load(Ordering::SeqCst), 2);

        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        let error = registry.available("flaky_contract").await.unwrap_err();
        assert_eq!(error.code, "temporarily_unavailable");
        assert_eq!(tool.probes.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn availability_ttl_and_last_good_start_after_slow_probe_completion() {
        let tool = Arc::new(
            ContractTool::transient_after_first_success("slow_flaky_contract")
                .with_first_probe_delay(Duration::from_secs(40)),
        );
        let registry = ToolRegistry::with_extra(vec![tool.clone()]);

        let first = registry
            .available("slow_flaky_contract")
            .await
            .unwrap()
            .unwrap();
        assert!(!first.stale);

        let immediate = registry
            .available("slow_flaky_contract")
            .await
            .unwrap()
            .unwrap();
        assert!(!immediate.stale);
        assert_eq!(tool.probes.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_secs(31)).await;
        let stale = registry
            .available("slow_flaky_contract")
            .await
            .unwrap()
            .unwrap();
        assert!(stale.stale);
        assert_eq!(tool.probes.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn hard_availability_failure_never_uses_last_good_grace() {
        let tool = Arc::new(
            ContractTool::transient_after_first_success("hard_failure_contract")
                .with_hard_failure(),
        );
        let registry = ToolRegistry::with_extra(vec![tool]);

        registry
            .available("hard_failure_contract")
            .await
            .unwrap()
            .unwrap();
        tokio::time::advance(Duration::from_secs(31)).await;

        let error = registry
            .available("hard_failure_contract")
            .await
            .unwrap_err();
        assert_eq!(
            error.category,
            crate::harness::native::tool_contract::ToolErrorCategory::Precondition
        );
    }

    #[tokio::test]
    async fn blocked_probe_does_not_serialize_unrelated_canonical_keys() {
        let blocking = Arc::new(BlockingProbeTool {
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let independent = Arc::new(ContractTool::new(
            "independent_probe",
            "independent",
            serde_json::json!({"type": "object"}),
        ));
        let registry = Arc::new(ToolRegistry::with_extra(vec![
            blocking.clone(),
            independent,
        ]));
        let blocking_lookup = {
            let registry = registry.clone();
            tokio::spawn(async move { registry.available("blocking_probe").await })
        };
        blocking.started.notified().await;

        let independent_result = tokio::time::timeout(
            Duration::from_secs(1),
            registry.available("independent_probe"),
        )
        .await
        .expect("unrelated canonical key should not wait for blocking probe")
        .unwrap();
        assert!(independent_result.is_some());

        blocking.release.notify_one();
        assert!(blocking_lookup.await.unwrap().unwrap().is_some());
    }

    #[test]
    fn tool_output_preserves_structured_errors_compatibly() {
        let ok = ToolOutput::ok("ok");
        assert!(ok.structured_error.is_none());

        let legacy = ToolOutput::error("legacy failure");
        assert_eq!(legacy.structured_error.unwrap().code, "tool_failed");

        let error = ToolError::caller("bad_argument", "Bad argument");
        let output = ToolOutput::from_error(error.clone());
        assert!(output.is_error);
        assert_eq!(output.for_model, "Bad argument");
        assert_eq!(output.structured_error, Some(error));
    }
}
