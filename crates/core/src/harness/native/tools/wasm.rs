//! WASM component connector-tool wrapper: exposes one component-provided tool
//! (`ryuzi:connector/connector`'s `list-tools`/`invoke`) as a native [`Tool`]
//! named `wasm__<component>__<tool>`.
//!
//! Deliberately mirrors [`super::extension::ExtensionTool`]: same `Tool` impl
//! shape and the same "a failing call becomes a tool ERROR, never a
//! propagated `Err`/panic/hang" guarantee. The only differences are WHERE the
//! call goes (a component's in-process `invoke` via
//! [`crate::plugins::wasm_connector::WasmActivation`], instead of an
//! extension subprocess's `tool/call`) and that every component tool carries a
//! resolved plugin [`Principal`] unconditionally (a component always belongs
//! to exactly one plugin).

use super::{truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::domain::Principal;
use crate::plugins::wasm_connector::{wasm_tool_name, WasmActivation, WasmToolBinding};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// A native tool backed by an enabled WASM component's connector export.
pub struct WasmTool {
    /// The `wasm__<component>__<tool>` name exposed to the model.
    full_name: String,
    /// The bare WIT tool name passed to the component's `invoke`.
    tool_name: String,
    description: String,
    schema: Value,
    principal: Principal,
    activation: Arc<WasmActivation>,
}

impl WasmTool {
    /// Wrap one gathered [`WasmToolBinding`]
    /// (`WasmTools::session_tools`) as a native `Tool`. Naming goes through
    /// `plugins::wasm_connector::wasm_tool_name` — the same helper
    /// `session_tools`'s own dedup uses — so the two can never drift.
    pub fn from_binding(binding: WasmToolBinding) -> WasmTool {
        WasmTool {
            full_name: wasm_tool_name(&binding.component_id, &binding.def.name),
            tool_name: binding.tool_name,
            description: binding.def.description,
            schema: binding.def.input_schema,
            principal: binding.principal,
            activation: binding.activation,
        }
    }
}

#[async_trait]
impl Tool for WasmTool {
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
        // Key on the tool's own full name so approval rules are per-tool,
        // mirroring `ExtensionTool::permission`.
        PermissionSpec::new(self.full_name.clone(), format!("run {}", self.full_name))
            .with_principal(Some(self.principal.clone()))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        match self
            .activation
            .connector_invoke(&self.tool_name, input)
            .await
        {
            Ok(value) => {
                // A plain string result is surfaced as raw text; anything else
                // is rendered as compact JSON for the model.
                let text = match value {
                    Value::String(text) => text,
                    other => serde_json::to_string(&other).unwrap_or_default(),
                };
                Ok(ToolOutput {
                    for_model: truncate(&text, &ctx.caps),
                    model_blocks: None,
                    display: None,
                    is_error: false,
                    structured_error: None,
                })
            }
            // A trapping/timing-out/rejecting component becomes a normal tool
            // ERROR, never a propagated `Err`/panic/hang — mirrors
            // `ExtensionTool::execute`'s own fallback exactly.
            Err(error) => Ok(ToolOutput::error(format!("{}: {error}", self.full_name))),
        }
    }
}
