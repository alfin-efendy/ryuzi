//! `todowrite` / `todoread` — the native runtime's per-session task list,
//! persisted to the `todos` table.

use super::{PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};

/// One todo item.
struct Item {
    content: String,
    status: String,
}

fn parse_items(input: &Value) -> Vec<Item> {
    input
        .get("todos")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let content = t.get("content").and_then(|v| v.as_str())?.to_string();
                    let status = t
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("pending")
                        .to_string();
                    Some(Item { content, status })
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn replace_todos(ctx: &ToolCtx, items: Vec<Item>) -> anyhow::Result<()> {
    let session_pk = ctx.session_pk.clone();
    let created = crate::paths::now_ms();
    ctx.store
        .with_conn(move |c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM todos WHERE session_pk=?1", [&session_pk])?;
            for (pos, item) in items.iter().enumerate() {
                tx.execute(
                    "INSERT INTO todos(session_pk,pos,content,status,created_at) \
                     VALUES (?1,?2,?3,?4,?5)",
                    rusqlite::params![session_pk, pos as i64, item.content, item.status, created],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
}

async fn load_todos(ctx: &ToolCtx) -> anyhow::Result<Vec<(String, String)>> {
    let session_pk = ctx.session_pk.clone();
    ctx.store
        .with_conn(move |c| -> rusqlite::Result<Vec<(String, String)>> {
            let mut stmt =
                c.prepare("SELECT content, status FROM todos WHERE session_pk=?1 ORDER BY pos")?;
            let rows = stmt
                .query_map([&session_pk], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
}

fn render(items: &[(String, String)]) -> String {
    if items.is_empty() {
        return "(no todos)".to_string();
    }
    items
        .iter()
        .map(|(content, status)| {
            let mark = match status.as_str() {
                "completed" => "[x]",
                "in_progress" => "[~]",
                _ => "[ ]",
            };
            format!("{mark} {content}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub struct TodoWrite;

#[async_trait]
impl Tool for TodoWrite {
    fn name(&self) -> &str {
        "todowrite"
    }
    fn description(&self) -> &str {
        "Replace the session's todo list. Pass the complete desired list each \
         time; it overwrites the previous one. Use this to plan and track \
         multi-step work."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {"type": "string"},
                            "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        })
    }
    fn kind(&self) -> &'static str {
        "other"
    }
    fn permission(&self, _input: &Value) -> PermissionSpec {
        PermissionSpec::new("todowrite", "update todo list")
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let items = parse_items(&input);
        let total = items.len();
        let done = items.iter().filter(|i| i.status == "completed").count();
        replace_todos(ctx, items).await?;
        let listing = render(&load_todos(ctx).await.unwrap_or_default());
        Ok(ToolOutput {
            for_model: format!("Updated todo list ({done}/{total} done):\n{listing}"),
            model_blocks: None,
            // A status block so the Cockpit UI shows progress.
            display: Some(json!({ "summary": format!("todos: {done}/{total} done") })),
            is_error: false,
        })
    }
}

pub struct TodoRead;

#[async_trait]
impl Tool for TodoRead {
    fn name(&self) -> &str {
        "todoread"
    }
    fn description(&self) -> &str {
        "Read the session's current todo list."
    }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn kind(&self) -> &'static str {
        "other"
    }
    fn permission(&self, _input: &Value) -> PermissionSpec {
        PermissionSpec::new("todoread", "read todo list")
    }
    async fn execute(&self, ctx: &ToolCtx, _input: Value) -> anyhow::Result<ToolOutput> {
        let items = load_todos(ctx).await.unwrap_or_default();
        Ok(ToolOutput::ok(render(&items)))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;

    #[tokio::test]
    async fn write_then_read_roundtrips_and_reports_progress() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = TodoWrite
            .execute(
                &ctx,
                json!({"todos": [
                    {"content": "first", "status": "completed"},
                    {"content": "second", "status": "in_progress"}
                ]}),
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(out.display.unwrap()["summary"], "todos: 1/2 done");

        let read = TodoRead.execute(&ctx, json!({})).await.unwrap();
        assert!(read.for_model.contains("[x] first"));
        assert!(read.for_model.contains("[~] second"));
    }

    #[tokio::test]
    async fn write_replaces_previous_list() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        TodoWrite
            .execute(
                &ctx,
                json!({"todos": [{"content": "old", "status": "pending"}]}),
            )
            .await
            .unwrap();
        TodoWrite
            .execute(
                &ctx,
                json!({"todos": [{"content": "new", "status": "pending"}]}),
            )
            .await
            .unwrap();
        let read = TodoRead.execute(&ctx, json!({})).await.unwrap();
        assert!(read.for_model.contains("new"));
        assert!(!read.for_model.contains("old"));
    }
}
