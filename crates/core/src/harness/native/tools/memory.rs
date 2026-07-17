//! Persistent-memory compatibility and V2 split facades.

use super::{PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::harness::native::memory as mem;
use crate::harness::native::tool_contract::{
    ToolDescriptor, ToolEffect, ToolError, ToolPolicyGroup,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub struct MemoryTool;
pub struct MemoryAdd;
pub struct MemoryReplace;
pub struct MemoryRemove;
pub struct MemoryBatch;

const DESCRIPTION: &str = "Persist durable facts across sessions. Scopes are global, user, and project. Do not store secrets.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum FacadeVersion {
    V1,
    V2,
}

struct MemoryCallError {
    legacy_message: String,
    typed: ToolError,
}

impl MemoryCallError {
    fn caller(code: &str, legacy_message: impl Into<String>) -> Self {
        Self {
            legacy_message: legacy_message.into(),
            typed: ToolError::caller(code, "Invalid memory input"),
        }
    }

    fn precondition(code: &str, legacy_message: impl Into<String>) -> Self {
        Self {
            legacy_message: legacy_message.into(),
            typed: ToolError::precondition(code, "Memory precondition was not met"),
        }
    }

    fn internal(legacy_message: impl Into<String>) -> Self {
        Self {
            legacy_message: legacy_message.into(),
            typed: ToolError::internal("memory_store_failed", "Memory update failed"),
        }
    }
}

fn failure(version: FacadeVersion, error: MemoryCallError) -> ToolOutput {
    match version {
        FacadeVersion::V1 => ToolOutput::error(error.legacy_message),
        FacadeVersion::V2 => ToolOutput::from_error(error.typed),
    }
}

fn parse_scope(value: &Value) -> Result<mem::MemoryScope, MemoryCallError> {
    let Some(raw) = value.get("scope").and_then(Value::as_str) else {
        return Err(MemoryCallError::caller(
            "invalid_memory_scope",
            "memory: `scope` is required (global|user|project)",
        ));
    };
    mem::MemoryScope::parse(raw)
        .map_err(|error| MemoryCallError::caller("invalid_memory_scope", error.to_string()))
}

fn string_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn parse_operation(
    value: &Value,
    forced_action: Option<&str>,
    version: FacadeVersion,
) -> Result<mem::MemoryOperation, MemoryCallError> {
    let action = forced_action.or_else(|| value.get("action").and_then(Value::as_str));
    let Some(action) = action else {
        return Err(MemoryCallError::caller(
            "invalid_memory_action",
            "memory: `action` is required (add|replace|remove)",
        ));
    };
    let scope = parse_scope(value)?;
    let text = string_field(value, "text");
    let matcher = string_field(value, "match");
    if version == FacadeVersion::V2 {
        if matches!(action, "add" | "replace") && text.trim().is_empty() {
            return Err(MemoryCallError::caller(
                "empty_memory_value",
                format!("memory {action}: `text` must not be empty"),
            ));
        }
        if matches!(action, "replace" | "remove") && matcher.trim().is_empty() {
            return Err(MemoryCallError::caller(
                "empty_memory_value",
                "memory: `match` must not be empty",
            ));
        }
    }
    match action {
        "add" => Ok(mem::MemoryOperation::Add { scope, text }),
        "replace" => Ok(mem::MemoryOperation::Replace {
            scope,
            matcher,
            text,
        }),
        "remove" => Ok(mem::MemoryOperation::Remove { scope, matcher }),
        other => Err(MemoryCallError::caller(
            "invalid_memory_action",
            format!("memory: unknown action `{other}` (add|replace|remove)"),
        )),
    }
}

fn map_store_error(error: anyhow::Error) -> MemoryCallError {
    let message = error.to_string();
    if message.contains("no project memory") {
        MemoryCallError::precondition("memory_project_unavailable", message)
    } else if message.contains("must not be empty") {
        MemoryCallError::caller("empty_memory_value", message)
    } else if message.contains("no entry contains") || message.contains(" matches ") {
        MemoryCallError::precondition("memory_match_not_unique", message)
    } else if message.contains("over budget") {
        MemoryCallError::precondition("memory_budget_exhausted", message)
    } else {
        MemoryCallError::internal(message)
    }
}

async fn execute_operations(
    ctx: &ToolCtx,
    operations: Vec<mem::MemoryOperation>,
    version: FacadeVersion,
) -> anyhow::Result<ToolOutput> {
    let Some(store) = &ctx.memory else {
        return Ok(failure(
            version,
            MemoryCallError::precondition(
                "memory_unavailable",
                "memory: unavailable in this context (sub-agents cannot write memory)",
            ),
        ));
    };
    let touched = operations
        .iter()
        .map(|operation| match operation {
            mem::MemoryOperation::Add { scope, .. }
            | mem::MemoryOperation::Replace { scope, .. }
            | mem::MemoryOperation::Remove { scope, .. } => *scope,
        })
        .collect::<BTreeSet<_>>();
    if let Err(error) = store.batch(operations).await {
        return Ok(failure(version, map_store_error(error)));
    }
    let mut summaries = Vec::new();
    for scope in touched {
        let entries = match store.load(scope).await {
            Ok(entries) => entries,
            Err(error) if version == FacadeVersion::V2 => {
                return Ok(failure(version, map_store_error(error)));
            }
            Err(error) => return Err(error),
        };
        summaries.push(format!(
            "{}: {} entries, {}/{} chars",
            scope.as_str(),
            entries.len(),
            mem::joined_chars(&entries),
            mem::BUDGET
        ));
    }
    let summary = summaries.join("; ");
    Ok(ToolOutput {
        for_model: format!("memory updated ({summary})"),
        model_blocks: None,
        display: Some(json!({ "summary": format!("memory: {summary}") })),
        is_error: false,
        structured_error: None,
    })
}

fn v1_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["add", "replace", "remove"]},
            "scope": {"type": "string", "enum": ["global", "user", "project"]},
            "text": {"type": "string", "description": "Entry text for add/replace."},
            "match": {"type": "string", "description": "Unique substring of the target entry for replace/remove."},
            "batch": {
                "type": "array",
                "minItems": 1,
                "description": "Multiple operations applied atomically (all or none).",
                "items": {
                    "type": "object",
                    "properties": {
                        "action": {"type": "string", "enum": ["add", "replace", "remove"]},
                        "scope": {"type": "string", "enum": ["global", "user", "project"]},
                        "text": {"type": "string"},
                        "match": {"type": "string"}
                    },
                    "required": ["action", "scope"]
                }
            }
        }
    })
}

fn scope_schema() -> Value {
    json!({"type":"string", "enum":["global", "user", "project"]})
}

fn explicit_schema(action: &str) -> Value {
    match action {
        "add" => json!({
            "type":"object",
            "properties":{
                "scope":scope_schema(),
                "text":{"type":"string", "minLength":1}
            },
            "required":["scope", "text"],
            "additionalProperties":false
        }),
        "replace" => json!({
            "type":"object",
            "properties":{
                "scope":scope_schema(),
                "match":{"type":"string", "minLength":1},
                "text":{"type":"string", "minLength":1}
            },
            "required":["scope", "match", "text"],
            "additionalProperties":false
        }),
        "remove" => json!({
            "type":"object",
            "properties":{
                "scope":scope_schema(),
                "match":{"type":"string", "minLength":1}
            },
            "required":["scope", "match"],
            "additionalProperties":false
        }),
        _ => unreachable!("explicit memory action"),
    }
}

fn batch_schema() -> Value {
    json!({
        "type":"object",
        "properties":{
            "operations":{
                "type":"array",
                "minItems":1,
                "items":{
                    "oneOf":[
                        {
                            "type":"object",
                            "properties":{
                                "action":{"type":"string", "enum":["add"]},
                                "scope":scope_schema(),
                                "text":{"type":"string", "minLength":1}
                            },
                            "required":["action", "scope", "text"],
                            "additionalProperties":false
                        },
                        {
                            "type":"object",
                            "properties":{
                                "action":{"type":"string", "enum":["replace"]},
                                "scope":scope_schema(),
                                "match":{"type":"string", "minLength":1},
                                "text":{"type":"string", "minLength":1}
                            },
                            "required":["action", "scope", "match", "text"],
                            "additionalProperties":false
                        },
                        {
                            "type":"object",
                            "properties":{
                                "action":{"type":"string", "enum":["remove"]},
                                "scope":scope_schema(),
                                "match":{"type":"string", "minLength":1}
                            },
                            "required":["action", "scope", "match"],
                            "additionalProperties":false
                        }
                    ]
                }
            }
        },
        "required":["operations"],
        "additionalProperties":false
    })
}

fn v2_descriptor(name: &str, action: &str, schema: Value) -> ToolDescriptor {
    let mut descriptor = ToolDescriptor::conservative(name, DESCRIPTION, schema, "other");
    descriptor.effect = ToolEffect::Mutating;
    descriptor.policy_aliases = vec!["memory".into()];
    descriptor.policy_groups = vec![ToolPolicyGroup {
        alias: "memory".into(),
        members: [
            "memory_add",
            "memory_batch",
            "memory_remove",
            "memory_replace",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    }];
    descriptor.v2_only = true;
    descriptor.description = format!("{DESCRIPTION} This facade performs `{action}` operations.");
    descriptor
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        "Persist durable facts across sessions (user preferences, environment \
         quirks, project conventions). Scopes: `global` (all projects), \
         `user` (about you — preferences/style), and `project`. Actions: \
         `add` new entry; `replace`/`remove` the single \
         entry containing `match` (a unique substring). Pass `batch` for \
         several operations applied atomically. Each scope has a hard \
         character budget — keep entries short and consolidate when told the \
         file is full. Do not store secrets."
    }

    fn input_schema(&self) -> Value {
        v1_schema()
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
        descriptor.effect = ToolEffect::Mutating;
        descriptor.v1_only = true;
        descriptor
    }

    fn permission(&self, input: &Value) -> PermissionSpec {
        let what = input
            .get("batch")
            .and_then(Value::as_array)
            .map(|batch| format!("{} batched updates", batch.len()))
            .or_else(|| {
                input
                    .get("action")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "update".into());
        PermissionSpec::new("memory", format!("persistent memory: {what}"))
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        if ctx.memory.is_none() {
            return Ok(ToolOutput::error(
                "memory: unavailable in this context (sub-agents cannot write memory)",
            ));
        }
        let operations = match input.get("batch").and_then(Value::as_array) {
            Some(batch) if batch.is_empty() => {
                return Ok(ToolOutput::error("memory: `batch` must not be empty"));
            }
            Some(batch) => batch
                .iter()
                .map(|value| parse_operation(value, None, FacadeVersion::V1))
                .collect(),
            None => {
                parse_operation(&input, None, FacadeVersion::V1).map(|operation| vec![operation])
            }
        };
        let operations = match operations {
            Ok(operations) => operations,
            Err(error) => return Ok(failure(FacadeVersion::V1, error)),
        };
        execute_operations(ctx, operations, FacadeVersion::V1).await
    }
}

macro_rules! explicit_memory_tool {
    ($tool:ty, $name:literal, $action:literal, $schema:expr) => {
        #[async_trait]
        impl Tool for $tool {
            fn name(&self) -> &str {
                $name
            }

            fn description(&self) -> &str {
                DESCRIPTION
            }

            fn input_schema(&self) -> Value {
                $schema
            }

            fn kind(&self) -> &'static str {
                "other"
            }

            fn descriptor(&self) -> ToolDescriptor {
                v2_descriptor(self.name(), $action, self.input_schema())
            }

            fn permission(&self, _input: &Value) -> PermissionSpec {
                PermissionSpec::new("memory", concat!("persistent memory: ", $action))
            }

            async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
                let operation = match parse_operation(&input, Some($action), FacadeVersion::V2) {
                    Ok(operation) => operation,
                    Err(error) => return Ok(failure(FacadeVersion::V2, error)),
                };
                execute_operations(ctx, vec![operation], FacadeVersion::V2).await
            }
        }
    };
}

explicit_memory_tool!(MemoryAdd, "memory_add", "add", explicit_schema("add"));
explicit_memory_tool!(
    MemoryReplace,
    "memory_replace",
    "replace",
    explicit_schema("replace")
);
explicit_memory_tool!(
    MemoryRemove,
    "memory_remove",
    "remove",
    explicit_schema("remove")
);

#[async_trait]
impl Tool for MemoryBatch {
    fn name(&self) -> &str {
        "memory_batch"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn input_schema(&self) -> Value {
        batch_schema()
    }

    fn kind(&self) -> &'static str {
        "other"
    }

    fn descriptor(&self) -> ToolDescriptor {
        v2_descriptor(self.name(), "batch", self.input_schema())
    }

    fn permission(&self, input: &Value) -> PermissionSpec {
        let count = input
            .get("operations")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        PermissionSpec::new(
            "memory",
            format!("persistent memory: {count} batched updates"),
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let Some(operations) = input.get("operations").and_then(Value::as_array) else {
            return Ok(failure(
                FacadeVersion::V2,
                MemoryCallError::caller(
                    "empty_memory_batch",
                    "memory: `operations` must not be empty",
                ),
            ));
        };
        if operations.is_empty() {
            return Ok(failure(
                FacadeVersion::V2,
                MemoryCallError::caller(
                    "empty_memory_batch",
                    "memory: `operations` must not be empty",
                ),
            ));
        }
        let operations = match operations
            .iter()
            .map(|value| parse_operation(value, None, FacadeVersion::V2))
            .collect()
        {
            Ok(operations) => operations,
            Err(error) => return Ok(failure(FacadeVersion::V2, error)),
        };
        execute_operations(ctx, operations, FacadeVersion::V2).await
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;
    use crate::agents::knowledge::AgentKnowledgeStore;
    use crate::harness::native::memory::{MemoryScope, MemoryStore};
    use crate::harness::native::tool_contract::compile_openai_strict_schema;
    use crate::harness::native::tools::ToolRegistry;
    use std::sync::Arc;

    const V2_MEMORY_TOOLS: [&str; 4] = [
        "memory_add",
        "memory_batch",
        "memory_remove",
        "memory_replace",
    ];

    async fn ctx_with_memory(dir: &std::path::Path) -> super::super::ToolCtx {
        let mut ctx = ctx_at(dir).await;
        ctx.memory = Some(Arc::new(
            MemoryStore::for_agent(
                Arc::new(AgentKnowledgeStore::new(dir.to_path_buf())),
                "ryuzi",
                Some("p1"),
            )
            .unwrap(),
        ));
        ctx
    }

    #[tokio::test]
    async fn add_persists_and_reports_usage() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_memory(dir.path()).await;
        let out = MemoryTool
            .execute(
                &ctx,
                json!({"action": "add", "scope": "global", "text": "prefers bun"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("global: 1 entries"));
        assert_eq!(
            ctx.memory
                .as_ref()
                .unwrap()
                .load(MemoryScope::Global)
                .await
                .unwrap(),
            vec!["prefers bun"]
        );
    }

    #[tokio::test]
    async fn replace_then_remove_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_memory(dir.path()).await;
        for input in [
            json!({"action": "add", "scope": "project", "text": "uses vite"}),
            json!({"action": "replace", "scope": "project", "match": "vite", "text": "uses vite + tauri"}),
            json!({"action": "remove", "scope": "project", "match": "tauri"}),
        ] {
            let out = MemoryTool.execute(&ctx, input).await.unwrap();
            assert!(!out.is_error, "{}", out.for_model);
        }
        assert!(ctx
            .memory
            .as_ref()
            .unwrap()
            .load(MemoryScope::Project)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn batch_is_all_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_memory(dir.path()).await;
        let out = MemoryTool
            .execute(
                &ctx,
                json!({"batch": [
                    {"action": "add", "scope": "global", "text": "valid entry"},
                    {"action": "remove", "scope": "global", "match": "does-not-exist"}
                ]}),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(ctx
            .memory
            .as_ref()
            .unwrap()
            .load(MemoryScope::Global)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn add_without_text_errors() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_memory(dir.path()).await;
        let out = MemoryTool
            .execute(&ctx, json!({"action": "add", "scope": "global"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("must not be empty"));
    }

    #[tokio::test]
    async fn without_memory_ctx_is_a_clean_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = MemoryTool
            .execute(
                &ctx,
                json!({"action": "add", "scope": "global", "text": "x"}),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("unavailable"));
    }

    #[tokio::test]
    async fn v1_memory_unavailable_precedes_malformed_input_byte_compatibly() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let output = MemoryTool.execute(&ctx, json!({})).await.unwrap();

        assert!(output.is_error);
        assert_eq!(
            output.for_model,
            "memory: unavailable in this context (sub-agents cannot write memory)"
        );
    }

    #[tokio::test]
    async fn v1_memory_empty_batch_rejects_without_mutating_storage() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_memory(dir.path()).await;
        ctx.memory
            .as_ref()
            .unwrap()
            .add(MemoryScope::Global, "existing fact")
            .await
            .unwrap();

        let output = MemoryTool.execute(&ctx, json!({"batch":[]})).await.unwrap();

        assert!(output.is_error);
        assert_eq!(output.for_model, "memory: `batch` must not be empty");
        assert_eq!(
            ctx.memory.unwrap().load(MemoryScope::Global).await.unwrap(),
            ["existing fact"]
        );
    }

    #[test]
    fn v2_memory_facade_keeps_v1_definition_separate() {
        let registry = ToolRegistry::builtin();
        let v1_names = registry
            .definitions()
            .into_iter()
            .filter_map(|definition| definition["name"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();

        assert!(v1_names.iter().any(|name| name == "memory"));
        assert!(V2_MEMORY_TOOLS
            .iter()
            .all(|name| !v1_names.iter().any(|visible| visible == name)));
        assert!(registry.registered("memory").unwrap().descriptor.v1_only);
        for name in V2_MEMORY_TOOLS {
            let registered = registry.registered(name).expect("V2 memory tool");
            assert!(registered.descriptor.v2_only);
            assert_eq!(registered.descriptor.policy_aliases, ["memory"]);
            assert_eq!(registered.tool.permission(&json!({})).key, "memory");
        }
    }

    #[test]
    fn v2_memory_schemas_are_closed_non_empty_and_strict_compilable() {
        let registry = ToolRegistry::builtin();
        let is_valid = |name: &str, input: &Value| {
            registry
                .registered(name)
                .unwrap()
                .canonical_validator
                .as_ref()
                .unwrap()
                .is_valid(input)
        };

        assert!(is_valid(
            "memory_add",
            &json!({"scope":"global", "text":"fact"})
        ));
        assert!(!is_valid(
            "memory_add",
            &json!({"scope":"invalid", "text":"fact"})
        ));
        assert!(!is_valid(
            "memory_add",
            &json!({"scope":"global", "text":""})
        ));
        assert!(!is_valid(
            "memory_add",
            &json!({"scope":"global", "text":"fact", "extra":true})
        ));
        assert!(!is_valid(
            "memory_replace",
            &json!({"scope":"global", "match":"", "text":"fact"})
        ));
        assert!(!is_valid(
            "memory_replace",
            &json!({"scope":"global", "match":"old", "text":""})
        ));
        assert!(!is_valid(
            "memory_remove",
            &json!({"scope":"global", "match":""})
        ));
        assert!(!is_valid(
            "memory_replace",
            &json!({"scope":"global", "match":"old"})
        ));
        assert!(!is_valid("memory_remove", &json!({"scope":"global"})));
        assert!(!is_valid("memory_batch", &json!({"operations":[]})));
        assert!(is_valid(
            "memory_batch",
            &json!({"operations":[
                {"action":"add", "scope":"global", "text":"fact"},
                {"action":"replace", "scope":"user", "match":"old", "text":"new"},
                {"action":"remove", "scope":"project", "match":"stale"}
            ]})
        ));
        assert!(!is_valid(
            "memory_batch",
            &json!({"operations":[{"action":"add", "scope":"global", "text":"fact", "match":"not allowed"}]})
        ));

        let batch = registry.registered("memory_batch").unwrap();
        let strict = compile_openai_strict_schema(&batch.canonical_schema).unwrap();
        assert!(strict["properties"]["operations"]["items"]
            .get("oneOf")
            .is_none());
        assert_eq!(
            strict["properties"]["operations"]["items"]["anyOf"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        let canonical_branches = batch.canonical_schema["properties"]["operations"]["items"]
            ["oneOf"]
            .as_array()
            .unwrap();
        assert_eq!(
            canonical_branches[0]["required"],
            json!(["action", "scope", "text"])
        );
        assert_eq!(
            canonical_branches[1]["required"],
            json!(["action", "scope", "match", "text"])
        );
        assert_eq!(
            canonical_branches[2]["required"],
            json!(["action", "scope", "match"])
        );
        assert_eq!(
            registry.registered("memory").unwrap().canonical_schema["properties"]["batch"]
                ["minItems"],
            1
        );
    }

    #[tokio::test]
    async fn v2_memory_explicit_operations_match_legacy_semantics() {
        async fn run(dir: &std::path::Path, calls: &[(&str, Value)]) -> Vec<String> {
            let registry = ToolRegistry::builtin();
            let ctx = ctx_with_memory(dir).await;
            for (name, input) in calls {
                let output = registry
                    .get(name)
                    .expect("registered memory facade")
                    .execute(&ctx, input.clone())
                    .await
                    .unwrap();
                assert!(!output.is_error, "{}: {}", name, output.for_model);
            }
            ctx.memory.unwrap().load(MemoryScope::Global).await.unwrap()
        }

        let legacy_dir = tempfile::tempdir().unwrap();
        let explicit_dir = tempfile::tempdir().unwrap();
        let legacy = run(
            legacy_dir.path(),
            &[
                (
                    "memory",
                    json!({"action":"add", "scope":"global", "text":"old fact"}),
                ),
                (
                    "memory",
                    json!({"action":"replace", "scope":"global", "match":"old", "text":"new fact"}),
                ),
                (
                    "memory",
                    json!({"action":"remove", "scope":"global", "match":"new"}),
                ),
                (
                    "memory",
                    json!({"action":"add", "scope":"global", "text":"final fact"}),
                ),
            ],
        )
        .await;
        let explicit = run(
            explicit_dir.path(),
            &[
                ("memory_add", json!({"scope":"global", "text":"old fact"})),
                (
                    "memory_replace",
                    json!({"scope":"global", "match":"old", "text":"new fact"}),
                ),
                ("memory_remove", json!({"scope":"global", "match":"new"})),
                ("memory_add", json!({"scope":"global", "text":"final fact"})),
            ],
        )
        .await;

        assert_eq!(explicit, legacy);
    }

    #[tokio::test]
    async fn v2_memory_batch_is_atomic_and_empty_batch_is_typed() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_memory(dir.path()).await;
        let tool = ToolRegistry::builtin().get("memory_batch").unwrap();
        let empty = tool.execute(&ctx, json!({"operations":[]})).await.unwrap();
        assert_eq!(empty.structured_error.unwrap().code, "empty_memory_batch");

        let failed = tool
            .execute(
                &ctx,
                json!({"operations":[
                    {"action":"add", "scope":"global", "text":"temporary"},
                    {"action":"remove", "scope":"global", "match":"missing"}
                ]}),
            )
            .await
            .unwrap();
        assert_eq!(
            failed.structured_error.unwrap().code,
            "memory_match_not_unique"
        );
        assert!(ctx
            .memory
            .unwrap()
            .load(MemoryScope::Global)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn v2_memory_errors_are_stable_and_redacted() {
        async fn code(tool: &str, ctx: &ToolCtx, input: Value) -> String {
            let output = ToolRegistry::builtin()
                .get(tool)
                .unwrap()
                .execute(ctx, input)
                .await
                .unwrap();
            assert!(output.is_error);
            assert!(!output.for_model.contains('\\'));
            assert!(!output.for_model.contains("source"));
            output.structured_error.unwrap().code
        }

        let dir = tempfile::tempdir().unwrap();
        let bare = ctx_at(dir.path()).await;
        assert_eq!(
            code(
                "memory_add",
                &bare,
                json!({"scope":"global", "text":"fact"})
            )
            .await,
            "memory_unavailable"
        );

        let mut ctx = ctx_at(dir.path()).await;
        ctx.memory = Some(Arc::new(
            MemoryStore::for_agent(
                Arc::new(AgentKnowledgeStore::new(dir.path().to_path_buf())),
                "ryuzi",
                None,
            )
            .unwrap(),
        ));
        assert_eq!(
            code(
                "memory_add",
                &ctx,
                json!({"scope":"invalid", "text":"fact"})
            )
            .await,
            "invalid_memory_scope"
        );
        assert_eq!(
            code("memory_add", &ctx, json!({"text":"fact"})).await,
            "invalid_memory_scope"
        );
        assert_eq!(
            code("memory_add", &ctx, json!({"scope":"global", "text":""})).await,
            "empty_memory_value"
        );
        assert_eq!(
            code(
                "memory_add",
                &ctx,
                json!({"scope":"project", "text":"fact"})
            )
            .await,
            "memory_project_unavailable"
        );
        assert_eq!(
            code(
                "memory_add",
                &ctx,
                json!({"scope":"global", "text":"x".repeat(crate::harness::native::memory::BUDGET + 1)})
            )
            .await,
            "memory_budget_exhausted"
        );

        let add = ToolRegistry::builtin().get("memory_add").unwrap();
        for text in ["shared first", "shared second"] {
            let output = add
                .execute(&ctx, json!({"scope":"user", "text":text}))
                .await
                .unwrap();
            assert!(!output.is_error);
        }
        assert_eq!(
            code(
                "memory_replace",
                &ctx,
                json!({"scope":"user", "match":"shared", "text":"replacement"})
            )
            .await,
            "memory_match_not_unique"
        );
    }
}
