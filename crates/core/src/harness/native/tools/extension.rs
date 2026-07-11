//! Extension tool wrapper (Track D, DT6): exposes one extension-provided
//! tool as a native [`Tool`] named `ext__<extension>__<tool>`, executing it
//! by dispatching `tool/call` to the owning extension subprocess over the
//! same demultiplexing transport DT4's health ping and DT5's event dispatch
//! already share. Deliberately mirrors [`super::mcp::McpTool`]: same `Tool`
//! impl shape, same `render_tool_result` content flattening for the reply —
//! the only real differences are WHERE the call is dispatched (an
//! extension's `tool/call`, not an MCP server's `tools/call`) and that every
//! extension tool carries a resolved plugin [`Principal`] unconditionally
//! (an extension always belongs to exactly one plugin, unlike an MCP server,
//! which may be DB-configured with no owning plugin at all).

use super::{truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::domain::Principal;
use crate::harness::native::mcp_client::render_tool_result;
use crate::plugins::extension::ExtensionToolBinding;
use async_trait::async_trait;
use serde_json::Value;

/// A native tool backed by a live extension subprocess (Track D, DT6).
pub struct ExtensionTool {
    /// The `ext__<extension>__<tool>` name exposed to the model.
    full_name: String,
    /// The bare tool name sent to the extension.
    tool_name: String,
    description: String,
    schema: Value,
    /// The owning plugin — always present (unlike `McpTool`'s optional
    /// principal): an `[[extension]]` is always declared by exactly one
    /// `CorePlugin`, resolved from that binding at `ExtensionHost::spawn_all`
    /// (`plugins::extension::proc`), never parsed from `full_name`/`tool_name`.
    principal: Principal,
    caller: std::sync::Arc<dyn crate::plugins::extension::ExtensionCaller>,
}

impl ExtensionTool {
    /// Wrap one gathered [`ExtensionToolBinding`] (DT6's
    /// `ExtensionTools::session_tools`) as a native `Tool`.
    pub fn from_binding(binding: ExtensionToolBinding) -> ExtensionTool {
        ExtensionTool {
            full_name: format!("ext__{}__{}", binding.extension_name, binding.def.name),
            tool_name: binding.def.name,
            description: binding.def.description,
            schema: binding.def.input_schema,
            principal: binding.principal,
            caller: binding.caller,
        }
    }
}

#[async_trait]
impl Tool for ExtensionTool {
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
        // Key on the tool's own full name (`ext__extension__tool`) — not a
        // shared "extension" bucket — so "always allow"/"always deny" rules
        // are per-tool, mirroring `McpTool::permission`'s own discipline.
        PermissionSpec::new(self.full_name.clone(), format!("run {}", self.full_name))
            .with_principal(Some(self.principal.clone()))
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
            // A dead/timed-out/rejecting extension becomes a normal tool
            // ERROR, never a propagated `Err`/panic/hang — mirrors
            // `McpTool::execute`'s own fallback exactly.
            Err(e) => Ok(ToolOutput::error(format!("{}: {e}", self.full_name))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;
    use crate::plugins::extension::{ExtensionCaller, ExtensionToolDef};
    use serde_json::json;
    use std::sync::Arc;

    struct FakeCaller {
        reply: Value,
    }
    #[async_trait]
    impl ExtensionCaller for FakeCaller {
        async fn call(&self, tool: &str, _arguments: Value) -> anyhow::Result<Value> {
            if tool == "boom" {
                anyhow::bail!("extension exploded");
            }
            Ok(self.reply.clone())
        }
    }

    fn binding(
        extension_name: &str,
        tool: &str,
        caller: Arc<dyn ExtensionCaller>,
    ) -> ExtensionToolBinding {
        ExtensionToolBinding {
            def: ExtensionToolDef {
                name: tool.to_string(),
                description: format!("run {tool}"),
                input_schema: json!({ "type": "object" }),
            },
            extension_name: extension_name.to_string(),
            principal: Principal {
                plugin_id: "acme-linter".into(),
                plugin_name: "Acme Linter".into(),
            },
            caller,
        }
    }

    #[tokio::test]
    async fn extension_tool_name_and_execution() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let caller = Arc::new(FakeCaller {
            reply: json!({ "content": [{"type": "text", "text": "0 problems"}] }),
        });
        let tool = ExtensionTool::from_binding(binding("linter", "lint", caller));

        assert_eq!(tool.name(), "ext__linter__lint");
        let out = tool.execute(&ctx, json!({ "path": "x.rs" })).await.unwrap();
        assert!(!out.is_error);
        assert_eq!(out.for_model, "0 problems");
    }

    #[tokio::test]
    async fn extension_tool_surfaces_caller_error_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let caller = Arc::new(FakeCaller { reply: json!({}) });
        let tool = ExtensionTool::from_binding(binding("linter", "boom", caller));

        let out = tool.execute(&ctx, json!({})).await.unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("extension exploded"));
    }

    #[tokio::test]
    async fn permission_carries_the_owning_plugin_principal() {
        let caller = Arc::new(FakeCaller { reply: json!({}) });
        let tool = ExtensionTool::from_binding(binding("linter", "lint", caller));

        let principal = tool.permission(&json!({})).principal;
        assert_eq!(
            principal,
            Some(Principal {
                plugin_id: "acme-linter".into(),
                plugin_name: "Acme Linter".into(),
            })
        );
    }

    #[tokio::test]
    async fn permission_key_is_the_tools_own_full_name_not_a_shared_bucket() {
        let caller = Arc::new(FakeCaller { reply: json!({}) });
        let lint = ExtensionTool::from_binding(binding("linter", "lint", caller.clone()));
        let format_tool = ExtensionTool::from_binding(binding("linter", "format", caller));

        let lint_key = lint.permission(&json!({})).key;
        let format_key = format_tool.permission(&json!({})).key;
        assert_eq!(lint_key, "ext__linter__lint");
        assert_eq!(format_key, "ext__linter__format");
        assert_ne!(lint_key, format_key);
    }

    #[test]
    fn naming_uses_the_ext_prefix_and_never_collides_with_mcp_or_built_ins() {
        let caller = Arc::new(FakeCaller { reply: json!({}) });
        let tool = ExtensionTool::from_binding(binding("linter", "lint", caller));
        assert!(tool.name().starts_with("ext__"));
        assert!(!tool.name().starts_with("mcp__"));

        // No built-in tool name can ever collide with the `ext__` namespace.
        let registry = crate::harness::native::tools::ToolRegistry::builtin();
        assert!(registry.names().iter().all(|n| !n.starts_with("ext__")));
    }
}
