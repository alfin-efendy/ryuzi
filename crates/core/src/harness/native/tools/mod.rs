//! Built-in tool suite for the native runtime.
//!
//! Each [`Tool`] declares a name, a JSON-schema for its input (hand-written to
//! avoid a `schemars` dependency), a `tool_kind` for the Cockpit UI, a
//! per-call [`PermissionSpec`], and an async `execute`. The [`ToolRegistry`]
//! assembles the built-ins and produces the Anthropic `tools` array.
//!
//! All file-touching tools resolve paths through [`jail`], which confines them
//! to the session worktree (reusing the ACP fs sandbox), and cap their output
//! via [`truncate`].

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
pub mod mcp;
pub mod read;
pub mod task;
pub mod todo;
pub mod webfetch;
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

/// Spawns a sub-agent for the `task` tool. Implemented by the runner; `None`
/// inside a sub-agent's own `ToolCtx` (sub-agents cannot nest further).
#[async_trait]
pub trait SubagentSpawner: Send + Sync {
    /// Run `agent_type` on `prompt` to completion and return its final text.
    async fn run(&self, agent_type: &str, prompt: &str) -> anyhow::Result<String>;
    /// Names of agents that may be spawned (for the tool description/errors).
    fn available(&self) -> Vec<String>;
}

/// Everything a tool needs to run one call.
pub struct ToolCtx {
    pub session_pk: String,
    /// The session worktree — the sandbox jail root.
    pub work_dir: PathBuf,
    pub store: Arc<Store>,
    pub cancel: CancellationToken,
    pub caps: OutputCaps,
    /// Sub-agent spawner for the `task` tool; `None` disables spawning.
    pub spawn: Option<Arc<dyn SubagentSpawner>>,
}

/// The result of a tool call.
pub struct ToolOutput {
    /// Text replayed to the model as the `tool_result` content (already
    /// truncated to caps by the tool).
    pub for_model: String,
    /// Optional extra fields merged into the persisted `tool_call` payload for
    /// the UI (e.g. a status summary). `None` for most tools.
    pub display: Option<Value>,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(text: impl Into<String>) -> Self {
        ToolOutput {
            for_model: text.into(),
            display: None,
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        ToolOutput {
            for_model: text.into(),
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
            Arc::new(task::Task),
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

/// Resolve `rel` against the session worktree, rejecting any escape. Reuses
/// the ACP fs sandbox so both runtimes share one path-jail implementation.
pub fn jail(work_dir: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    crate::harness::acp::fs::sandbox(work_dir, Path::new(rel))
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
            store,
            cancel: CancellationToken::new(),
            caps: OutputCaps::default(),
            spawn: None,
        }
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
            "task",
        ] {
            assert!(reg.get(name).is_some(), "missing tool {name}");
        }
        let defs = reg.definitions();
        assert_eq!(defs.len(), 11);
        assert!(defs.iter().all(|d| d.get("name").is_some()
            && d.get("description").is_some()
            && d.get("input_schema").is_some()));
    }
}
