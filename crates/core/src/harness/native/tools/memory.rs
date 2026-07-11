//! `memory` — persist durable facts across sessions.
//!
//! Thin tool surface over [`crate::harness::native::memory::MemoryStore`]:
//! `add`/`replace`/`remove` on the global, user, or project scope, plus an
//! atomic `batch`. Sub-agents run with `ToolCtx.memory = None` (and the tool
//! filtered out), mirroring hermes-agent's `skip_memory` for children.

use super::{PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::harness::native::memory as mem;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub struct MemoryTool;

/// One parsed operation (single form or a batch element).
struct Op {
    action: String,
    scope: mem::MemoryScope,
    text: Option<String>,
    matcher: Option<String>,
}

fn parse_op(v: &Value) -> anyhow::Result<Op> {
    let action = v
        .get("action")
        .and_then(|a| a.as_str())
        .ok_or_else(|| anyhow::anyhow!("memory: `action` is required (add|replace|remove)"))?
        .to_string();
    let scope =
        mem::MemoryScope::parse(v.get("scope").and_then(|s| s.as_str()).ok_or_else(|| {
            anyhow::anyhow!("memory: `scope` is required (global|user|project)")
        })?)?;
    Ok(Op {
        action,
        scope,
        text: v.get("text").and_then(|t| t.as_str()).map(str::to_string),
        matcher: v.get("match").and_then(|m| m.as_str()).map(str::to_string),
    })
}

/// Apply one op to the in-memory entries for its scope.
fn apply(op: &Op, entries: &mut Vec<String>) -> anyhow::Result<()> {
    let text = op.text.as_deref().unwrap_or("");
    let matcher = op.matcher.as_deref().unwrap_or("");
    match op.action.as_str() {
        "add" => mem::add_entry(entries, text),
        "replace" => mem::replace_entry(entries, matcher, text),
        "remove" => mem::remove_entry(entries, matcher),
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
            .and_then(|b| b.as_array())
            .map(|b| format!("{} batched updates", b.len()))
            .or_else(|| {
                input
                    .get("action")
                    .and_then(|a| a.as_str())
                    .map(str::to_string)
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
        let ops: Vec<Op> = match input.get("batch").and_then(|b| b.as_array()) {
            Some(batch) if !batch.is_empty() => match batch.iter().map(parse_op).collect() {
                Ok(ops) => ops,
                Err(e) => return Ok(ToolOutput::error(e.to_string())),
            },
            Some(_) => return Ok(ToolOutput::error("memory: `batch` must not be empty")),
            None => match parse_op(&input) {
                Ok(op) => vec![op],
                Err(e) => return Ok(ToolOutput::error(e.to_string())),
            },
        };

        // Stage: load each touched scope once, apply every op in order, and
        // validate budgets — only then persist, so a failing op means nothing
        // was written (all-or-nothing batches). The whole read-modify-write
        // cycle runs under the process-wide memory lock so a concurrent
        // session cannot interleave a write between our load and save (this
        // block is fully synchronous — the guard never crosses an await).
        let _guard = mem::write_lock();
        let mut staged: BTreeMap<&'static str, (mem::MemoryScope, Vec<String>)> = BTreeMap::new();
        for op in &ops {
            staged
                .entry(op.scope.as_str())
                .or_insert_with(|| (op.scope, store.load(op.scope)));
        }
        for op in &ops {
            let (_, entries) = staged.get_mut(op.scope.as_str()).expect("staged scope");
            if let Err(e) = apply(op, entries) {
                return Ok(ToolOutput::error(e.to_string()));
            }
        }
        // Validate every touched scope before saving any of them, so a budget
        // failure in the second scope can't leave the first half-written.
        for (scope, entries) in staged.values() {
            if let Err(e) = mem::validate_budget(*scope, entries) {
                return Ok(ToolOutput::error(e.to_string()));
            }
        }
        for (scope, entries) in staged.values() {
            if let Err(e) = store.save(*scope, entries) {
                return Ok(ToolOutput::error(e.to_string()));
            }
        }

        let summary = staged
            .values()
            .map(|(scope, entries)| {
                format!(
                    "{}: {} entries, {}/{} chars",
                    scope.as_str(),
                    entries.len(),
                    mem::joined_chars(entries),
                    mem::BUDGET,
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
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
    use crate::harness::native::memory::{MemoryScope, MemoryStore};
    use std::sync::Arc;

    async fn ctx_with_memory(dir: &std::path::Path) -> super::super::ToolCtx {
        let mut ctx = ctx_at(dir).await;
        ctx.memory = Some(Arc::new(MemoryStore::new(
            dir.join("mem/MEMORY.md"),
            dir.join("mem/USER.md"),
            Some(dir.join("mem/projects/p1.md")),
        )));
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
        assert!(
            out.for_model.contains("global: 1 entries"),
            "{}",
            out.for_model
        );
        let mem = ctx.memory.as_ref().unwrap();
        assert_eq!(
            mem.load(MemoryScope::Global),
            vec!["prefers bun".to_string()]
        );
    }

    #[tokio::test]
    async fn add_persists_to_user_scope() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_memory(dir.path()).await;
        let out = MemoryTool
            .execute(
                &ctx,
                json!({"action": "add", "scope": "user", "text": "prefers terse answers"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(
            out.for_model.contains("user: 1 entries"),
            "{}",
            out.for_model
        );
        let mem = ctx.memory.as_ref().unwrap();
        assert_eq!(
            mem.load(MemoryScope::User),
            vec!["prefers terse answers".to_string()]
        );
    }

    #[tokio::test]
    async fn replace_then_remove_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_memory(dir.path()).await;
        for input in [
            json!({"action": "add", "scope": "project", "text": "uses vite"}),
            json!({"action": "replace", "scope": "project", "match": "vite", "text": "uses vite + tauri"}),
        ] {
            let out = MemoryTool.execute(&ctx, input).await.unwrap();
            assert!(!out.is_error, "{}", out.for_model);
        }
        let mem = ctx.memory.as_ref().unwrap();
        assert_eq!(
            mem.load(MemoryScope::Project),
            vec!["uses vite + tauri".to_string()]
        );
        let out = MemoryTool
            .execute(
                &ctx,
                json!({"action": "remove", "scope": "project", "match": "tauri"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(mem.load(MemoryScope::Project).is_empty());
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
        // The first (valid) op must not have been persisted.
        let mem = ctx.memory.as_ref().unwrap();
        assert!(mem.load(MemoryScope::Global).is_empty());
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
        assert!(
            out.for_model.contains("must not be empty"),
            "{}",
            out.for_model
        );
    }

    #[tokio::test]
    async fn without_memory_ctx_is_a_clean_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await; // memory: None
        let out = MemoryTool
            .execute(
                &ctx,
                json!({"action": "add", "scope": "global", "text": "x"}),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("unavailable"), "{}", out.for_model);
    }
}
