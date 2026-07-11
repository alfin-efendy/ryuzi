//! MCP tool wrapper: exposes one MCP server tool as a native [`Tool`] named
//! `mcp__<server>__<tool>`, executing it through an [`McpCaller`].

use super::{truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::harness::native::mcp_client::{render_tool_result, McpCaller};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// A native tool backed by a live MCP server connection.
pub struct McpTool {
    /// The `mcp__server__tool` name exposed to the model.
    full_name: String,
    /// The bare tool name sent to the MCP server.
    tool_name: String,
    description: String,
    schema: Value,
    caller: Arc<dyn McpCaller>,
}

impl McpTool {
    pub fn new(
        server: &str,
        tool_name: &str,
        description: &str,
        schema: Value,
        caller: Arc<dyn McpCaller>,
    ) -> McpTool {
        McpTool {
            full_name: format!("mcp__{server}__{tool_name}"),
            tool_name: tool_name.to_string(),
            description: description.to_string(),
            schema,
            caller,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.full_name
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
    fn permission(&self, _input: &Value) -> PermissionSpec {
        // Key on the tool's own full name (`mcp__server__tool`) — not a
        // shared "mcp" bucket — so "always allow"/"always deny" rules are
        // per-tool, not a blanket rule covering every MCP tool from every
        // server. `key_to_policy_tool` passes unknown keys through
        // unchanged, so this needs no other plumbing changes.
        PermissionSpec::new(self.full_name.clone(), format!("run {}", self.full_name))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        match self.caller.call(&self.tool_name, input).await {
            Ok(result) => {
                let (text, is_error) = render_tool_result(&result);
                Ok(ToolOutput {
                    for_model: truncate(&text, &ctx.caps),
                    model_blocks: None,
                    display: None,
                    is_error,
                })
            }
            Err(e) => Ok(ToolOutput::error(format!("{}: {e}", self.full_name))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;
    use serde_json::json;

    struct FakeCaller {
        reply: Value,
    }
    #[async_trait]
    impl McpCaller for FakeCaller {
        async fn call(&self, tool: &str, _arguments: Value) -> anyhow::Result<Value> {
            if tool == "boom" {
                anyhow::bail!("server exploded");
            }
            Ok(self.reply.clone())
        }
    }

    #[tokio::test]
    async fn mcp_tool_name_and_execution() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let caller = Arc::new(FakeCaller {
            reply: json!({ "content": [{"type": "text", "text": "42 results"}] }),
        });
        let tool = McpTool::new(
            "notion",
            "search",
            "Search Notion",
            json!({ "type": "object" }),
            caller,
        );
        assert_eq!(tool.name(), "mcp__notion__search");
        let out = tool.execute(&ctx, json!({ "q": "x" })).await.unwrap();
        assert!(!out.is_error);
        assert_eq!(out.for_model, "42 results");
    }

    #[tokio::test]
    async fn permission_key_is_the_tools_own_full_name_not_a_shared_bucket() {
        let caller = Arc::new(FakeCaller { reply: json!({}) });
        let search = McpTool::new(
            "notion",
            "search",
            "Search Notion",
            json!({}),
            caller.clone(),
        );
        let fetch = McpTool::new("github", "fetch_issue", "Fetch issue", json!({}), caller);

        let search_key = search.permission(&json!({})).key;
        let fetch_key = fetch.permission(&json!({})).key;

        assert_eq!(search_key, "mcp__notion__search");
        assert_eq!(fetch_key, "mcp__github__fetch_issue");
        assert_ne!(
            search_key, fetch_key,
            "two different MCP tools must get two different permission keys, \
             so an 'always allow'/'always deny' rule for one never covers the other"
        );
    }

    #[tokio::test]
    async fn mcp_tool_surfaces_caller_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let caller = Arc::new(FakeCaller { reply: json!({}) });
        let tool = McpTool::new("s", "boom", "d", json!({}), caller);
        let out = tool.execute(&ctx, json!({})).await.unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("server exploded"));
    }
}
