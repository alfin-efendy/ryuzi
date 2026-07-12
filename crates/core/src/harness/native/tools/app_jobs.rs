//! `app_jobs` — the agent operates the cron scheduler (spec §9.1). Reads
//! auto-allow (`jobs.read`); mutations prompt (`jobs.write`) and are audited
//! inside the facade. Never available to sub-agents/workers (`ctx.app` None).

use super::{AppJobCreate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct AppJobs;

fn action(input: &Value) -> &str {
    input.get("action").and_then(|a| a.as_str()).unwrap_or("")
}

#[async_trait]
impl Tool for AppJobs {
    fn name(&self) -> &str {
        "app_jobs"
    }
    fn description(&self) -> &str {
        "Operate the app's scheduled jobs: list existing jobs, create a new one \
         (natural-language schedule like 'every day at 9am' or a cron expr), \
         pause/resume a job, or run one now. Use this to automate recurring work."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "create", "pause", "resume", "run"]},
                "id": {"type": "string", "description": "job id (pause/resume/run)"},
                "name": {"type": "string", "description": "job name (create)"},
                "schedule": {"type": "string", "description": "natural language or cron (create)"},
                "prompt": {"type": "string", "description": "the prompt the job runs (create)"},
                "project_id": {"type": "string"},
                "model_override": {"type": "string"}
            },
            "required": ["action"]
        })
    }
    fn kind(&self) -> &'static str {
        "other"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        match action(input) {
            "list" => PermissionSpec::new("jobs.read", "list scheduled jobs"),
            other => PermissionSpec::new("jobs.write", format!("{other} a scheduled job")),
        }
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let Some(app) = ctx.app.as_ref() else {
            return Ok(ToolOutput::error(
                "app_jobs is not available in this context",
            ));
        };
        match action(&input) {
            "list" => {
                let jobs = app.list_jobs().await?;
                if jobs.is_empty() {
                    return Ok(ToolOutput::ok("No scheduled jobs."));
                }
                let lines: Vec<String> = jobs
                    .iter()
                    .map(|j| {
                        format!(
                            "- {} [{}] {} ({})",
                            j.name,
                            j.id,
                            j.cron,
                            if j.enabled { "enabled" } else { "paused" }
                        )
                    })
                    .collect();
                Ok(ToolOutput::ok(lines.join("\n")))
            }
            "create" => {
                let name = input
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let schedule = input
                    .get("schedule")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let prompt = input
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() || schedule.is_empty() || prompt.is_empty() {
                    return Ok(ToolOutput::error(
                        "create requires non-empty 'name', 'schedule', and 'prompt'",
                    ));
                }
                let id = app
                    .create_job(AppJobCreate {
                        name,
                        schedule,
                        prompt,
                        project_id: input
                            .get("project_id")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        model_override: input
                            .get("model_override")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                    })
                    .await?;
                Ok(ToolOutput::ok(format!("Created job {id}.")))
            }
            act @ ("pause" | "resume") => {
                let Some(id) = input.get("id").and_then(|v| v.as_str()) else {
                    return Ok(ToolOutput::error("pause/resume requires 'id'"));
                };
                let ok = app.set_job_enabled(id, act == "resume").await?;
                Ok(ToolOutput::ok(if ok {
                    format!("Job {id} {act}d.")
                } else {
                    format!("No such job {id}.")
                }))
            }
            "run" => {
                let Some(id) = input.get("id").and_then(|v| v.as_str()) else {
                    return Ok(ToolOutput::error("run requires 'id'"));
                };
                let run = app.run_job_now(id).await?;
                Ok(ToolOutput::ok(format!("Started run {run} for job {id}.")))
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
    async fn create_action_calls_facade_and_reports_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ctx_at(dir.path()).await;
        let fake = Arc::new(FakeAppControl::default());
        ctx.app = Some(fake.clone());
        let out = AppJobs
            .execute(
                &ctx,
                json!({"action": "create", "name": "nightly",
                       "schedule": "every day at 9am", "prompt": "summarize"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("job-new"));
        assert_eq!(fake.created.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn list_action_formats_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ctx_at(dir.path()).await;
        ctx.app = Some(Arc::new(FakeAppControl::default()));
        let out = AppJobs
            .execute(&ctx, json!({"action": "list"}))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("nightly"));
    }

    #[tokio::test]
    async fn missing_facade_is_not_available() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await; // app: None
        let out = AppJobs
            .execute(&ctx, json!({"action": "list"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("not available"));
    }

    #[test]
    fn permission_key_depends_on_action() {
        assert_eq!(
            AppJobs.permission(&json!({"action": "list"})).key,
            "jobs.read"
        );
        assert_eq!(
            AppJobs.permission(&json!({"action": "create"})).key,
            "jobs.write"
        );
    }
}
