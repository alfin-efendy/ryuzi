//! `memory` — persist durable facts across sessions.

use super::{PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::harness::native::memory as mem;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct MemoryTool;

fn parse_operation(value: &Value) -> anyhow::Result<mem::MemoryOperation> {
    let action = value
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("memory: `action` is required (add|replace|remove)"))?;
    let scope =
        mem::MemoryScope::parse(value.get("scope").and_then(Value::as_str).ok_or_else(|| {
            anyhow::anyhow!("memory: `scope` is required (global|user|project)")
        })?)?;
    let text = || {
        value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let matcher = || {
        value
            .get("match")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    match action {
        "add" => Ok(mem::MemoryOperation::Add {
            scope,
            text: text(),
        }),
        "replace" => Ok(mem::MemoryOperation::Replace {
            scope,
            matcher: matcher(),
            text: text(),
        }),
        "remove" => Ok(mem::MemoryOperation::Remove {
            scope,
            matcher: matcher(),
        }),
        other => anyhow::bail!("memory: unknown action `{other}` (add|replace|remove)"),
    }
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
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["add", "replace", "remove"]},
                "scope": {"type": "string", "enum": ["global", "user", "project"]},
                "text": {"type": "string", "description": "Entry text for add/replace."},
                "match": {"type": "string", "description": "Unique substring of the target entry for replace/remove."},
                "batch": {
                    "type": "array",
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

    fn kind(&self) -> &'static str {
        "other"
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
        let Some(store) = &ctx.memory else {
            return Ok(ToolOutput::error(
                "memory: unavailable in this context (sub-agents cannot write memory)",
            ));
        };
        let operations = match input.get("batch").and_then(Value::as_array) {
            Some(batch) if batch.is_empty() => {
                return Ok(ToolOutput::error("memory: `batch` must not be empty"));
            }
            Some(batch) => batch.iter().map(parse_operation).collect(),
            None => parse_operation(&input).map(|operation| vec![operation]),
        };
        let operations = match operations {
            Ok(operations) => operations,
            Err(error) => return Ok(ToolOutput::error(error.to_string())),
        };
        let touched = operations
            .iter()
            .map(|operation| match operation {
                mem::MemoryOperation::Add { scope, .. }
                | mem::MemoryOperation::Replace { scope, .. }
                | mem::MemoryOperation::Remove { scope, .. } => *scope,
            })
            .collect::<std::collections::BTreeSet<_>>();
        if let Err(error) = store.batch(operations).await {
            return Ok(ToolOutput::error(error.to_string()));
        }
        let mut summaries = Vec::new();
        for scope in touched {
            let entries = store.load(scope).await?;
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
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;
    use crate::agents::knowledge::AgentKnowledgeStore;
    use crate::harness::native::memory::{MemoryScope, MemoryStore};
    use std::sync::Arc;

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
}
