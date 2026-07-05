//! `task` — delegate a subtask to a sub-agent.
//!
//! The runner supplies a [`super::SubagentSpawner`] in the `ToolCtx`; this tool
//! resolves the requested `subagent_type`, runs it to completion in an
//! isolated (ephemeral-history) sub-loop, and returns its final report as the
//! tool result. Sub-agents cannot spawn further sub-agents (their `ToolCtx`
//! carries no spawner).

use super::{PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct Task;

#[async_trait]
impl Tool for Task {
    fn name(&self) -> &'static str {
        "task"
    }
    fn description(&self) -> &'static str {
        "Delegate a self-contained subtask to a sub-agent. Provide a precise, \
         standalone `prompt` (the sub-agent does not see this conversation) and \
         a `subagent_type` (e.g. `general` for multi-step work, `explore` for \
         read-only investigation). Returns the sub-agent's final report."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "description": {"type": "string", "description": "A short (3-5 word) label for the subtask."},
                "prompt": {"type": "string", "description": "The full, self-contained task for the sub-agent."},
                "subagent_type": {"type": "string", "description": "Which sub-agent to use (e.g. general, explore)."}
            },
            "required": ["prompt", "subagent_type"]
        })
    }
    fn kind(&self) -> &'static str {
        "other"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let ty = input
            .get("subagent_type")
            .and_then(|v| v.as_str())
            .unwrap_or("agent");
        PermissionSpec::new("task", format!("delegate to {ty}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let Some(spawner) = &ctx.spawn else {
            return Ok(ToolOutput::error(
                "task: sub-agents cannot spawn further sub-agents",
            ));
        };
        let prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("task: `prompt` is required"))?;
        let ty = input
            .get("subagent_type")
            .and_then(|v| v.as_str())
            .unwrap_or("general");
        match spawner.run(ty, prompt).await {
            Ok(report) => Ok(ToolOutput::ok(report)),
            Err(e) => Ok(ToolOutput::error(format!(
                "task: sub-agent `{ty}` failed: {e} (available: {})",
                spawner.available().join(", ")
            ))),
        }
    }
}
