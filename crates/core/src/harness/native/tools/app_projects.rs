//! `app_projects` — the agent lists projects, opens a project-less chat, or
//! attaches a project to a session (spec §9.1, §5). Reads auto-allow
//! (`projects.read`); mutations prompt (`projects.write`).

use super::{PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct AppProjects;

fn action(input: &Value) -> &str {
    input.get("action").and_then(|a| a.as_str()).unwrap_or("")
}

#[async_trait]
impl Tool for AppProjects {
    fn name(&self) -> &str {
        "app_projects"
    }
    fn description(&self) -> &str {
        "Operate the app's projects: list projects, open a new project-less chat \
         session, or attach a project to a session so its files and memory are in \
         scope."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "create_chat", "attach"]},
                "title": {"type": "string", "description": "chat title (create_chat)"},
                "session_pk": {"type": "string", "description": "session to attach to (attach)"},
                "project_id": {"type": "string", "description": "project to attach (attach)"}
            },
            "required": ["action"]
        })
    }
    fn kind(&self) -> &'static str {
        "other"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        match action(input) {
            "list" => PermissionSpec::new("projects.read", "list projects"),
            other => PermissionSpec::new("projects.write", format!("{other} a project/session")),
        }
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let Some(app) = ctx.app.as_ref() else {
            return Ok(ToolOutput::error(
                "app_projects is not available in this context",
            ));
        };
        match action(&input) {
            "list" => {
                let projects = app.list_projects().await?;
                if projects.is_empty() {
                    return Ok(ToolOutput::ok("No projects."));
                }
                let lines: Vec<String> = projects
                    .iter()
                    .map(|p| format!("- {} [{}]", p.name, p.id))
                    .collect();
                Ok(ToolOutput::ok(lines.join("\n")))
            }
            "create_chat" => {
                let title = input
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let pk = app.create_chat_session(title).await?;
                Ok(ToolOutput::ok(format!("Opened chat session {pk}.")))
            }
            "attach" => {
                let (Some(session_pk), Some(project_id)) = (
                    input.get("session_pk").and_then(|v| v.as_str()),
                    input.get("project_id").and_then(|v| v.as_str()),
                ) else {
                    return Ok(ToolOutput::error(
                        "attach requires 'session_pk' and 'project_id'",
                    ));
                };
                app.attach_project(session_pk, project_id).await?;
                Ok(ToolOutput::ok(format!(
                    "Attached {project_id} to {session_pk}."
                )))
            }
            other => Ok(ToolOutput::error(format!("unknown action {other:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::native::tools::testutil::{ctx_at, FakeAppControl};
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn list_formats_projects_and_create_chat_returns_pk() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ctx_at(dir.path()).await;
        ctx.app = Some(Arc::new(FakeAppControl::default()));
        let out = AppProjects
            .execute(&ctx, json!({"action": "list"}))
            .await
            .unwrap();
        assert!(out.for_model.contains("Ryuzi"));
        let out = AppProjects
            .execute(&ctx, json!({"action": "create_chat", "title": "notes"}))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("chat-1"));
    }

    #[test]
    fn permission_keys() {
        assert_eq!(
            AppProjects.permission(&json!({"action": "list"})).key,
            "projects.read"
        );
        assert_eq!(
            AppProjects.permission(&json!({"action": "attach"})).key,
            "projects.write"
        );
    }
}
