//! Built-in tool suite for the native runtime.
//!
//! Each [`Tool`] declares a name, a JSON-schema for its input (hand-written to
//! avoid a `schemars` dependency), a `tool_kind` for the Cockpit UI, a
//! per-call [`PermissionSpec`], and an async `execute`. The [`ToolRegistry`]
//! assembles the built-ins and produces the Anthropic `tools` array.
//!
//! All file-touching tools resolve paths through [`jail`], which confines them
//! to the session worktree, and cap their output via [`truncate`].

use crate::store::Store;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub mod bash;
pub mod edit;
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
pub mod skill;
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
}

/// Channel bundle for tools whose EXECUTION is a user interaction
/// (`exitplanmode`, `askuserquestion`): they emit their own
/// `ApprovalRequested` and block on the reply, reusing the approval pipeline.
pub struct Interaction {
    pub approvals: Arc<crate::approval::ApprovalHub>,
    pub events: tokio::sync::broadcast::Sender<crate::domain::CoreEvent>,
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
        let rx = self
            .approvals
            .register_for_session(session_pk, request_id.to_string());
        let _ = self
            .events
            .send(crate::domain::CoreEvent::ApprovalRequested {
                session_pk: session_pk.to_string(),
                request_id: request_id.to_string(),
                tool: tool.to_string(),
                summary: summary.to_string(),
                approval_kind,
                input,
            });
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                self.approvals.resolve_bool(request_id, false);
                None
            }
            res = rx => res.ok(),
        }
    }
}

/// Everything a tool needs to run one call.
pub struct ToolCtx {
    pub session_pk: String,
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
}

impl ToolOutput {
    pub fn ok(text: impl Into<String>) -> Self {
        ToolOutput {
            for_model: text.into(),
            model_blocks: None,
            display: None,
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        ToolOutput {
            for_model: text.into(),
            model_blocks: None,
            display: None,
            is_error: true,
        }
    }
}

/// How a tool call is gated: a permission `key` (matched against `PermMode` /
/// project policy) and a human `summary` for the approval prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSpec {
    pub key: String,
    pub summary: String,
}

impl PermissionSpec {
    pub fn new(key: impl Into<String>, summary: impl Into<String>) -> Self {
        PermissionSpec {
            key: key.into(),
            summary: summary.into(),
        }
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

/// The set of tools available to a session, keyed by name. Built-ins plus any
/// per-session MCP tools.
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// All built-in tools.
    pub fn builtin() -> Self {
        let list: Vec<Arc<dyn Tool>> = vec![
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
            Arc::new(memory::MemoryTool),
            Arc::new(revert::Revert),
            Arc::new(lsp::Lsp),
            Arc::new(task::Task),
            Arc::new(plan::ExitPlanMode),
            Arc::new(question::AskUserQuestion),
        ];
        let mut tools = BTreeMap::new();
        for t in list {
            tools.insert(t.name().to_string(), t);
        }
        ToolRegistry { tools }
    }

    /// The built-ins plus a set of extra (e.g. MCP) tools.
    pub fn with_extra(extra: Vec<Arc<dyn Tool>>) -> Self {
        let mut reg = Self::builtin();
        for t in extra {
            reg.tools.insert(t.name().to_string(), t);
        }
        reg
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// The Anthropic `tools` array for a provider request.
    pub fn definitions(&self) -> Vec<Value> {
        self.tools.values().map(|t| t.definition()).collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
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

    // Quick check on the lexically normalized path before canonicalization.
    // An absolute path that isn't a prefix of canonical_root after normalization
    // is definitely an escape.
    if !normalized.starts_with(&canonical_root) {
        anyhow::bail!(
            "sandbox: path {} escapes the worktree {}",
            path.display(),
            canonical_root.display()
        );
    }

    // Now canonicalize the deepest existing ancestor to resolve any symlinks in
    // the directory chain and re-verify. Walk upward until we find an extant dir.
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
            work_dir: dir.to_path_buf(),
            attachments_dir: None,
            extra_skill_dirs: vec![],
            store,
            cancel: CancellationToken::new(),
            caps: OutputCaps::default(),
            spawn: None,
            memory: None,
            snapshots: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            tool_call_id: "test-call".into(),
            interaction: None,
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
            perm_mode: perm.clone(),
            project_id: None,
        }));
        (ctx, hub, rx, perm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
            "memory",
            "revert",
            "lsp",
            "task",
            "exitplanmode",
            "askuserquestion",
        ] {
            assert!(reg.get(name).is_some(), "missing tool {name}");
        }
        let defs = reg.definitions();
        assert_eq!(defs.len(), 18);
        assert!(defs.iter().all(|d| d.get("name").is_some()
            && d.get("description").is_some()
            && d.get("input_schema").is_some()));
    }
}
