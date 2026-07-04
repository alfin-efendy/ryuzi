//! Scheduler screen commands. Jobs persist in SQLite; the core runner loop
//! fires them for real (starting agent sessions); run history closes off the
//! session's Result/Error events.

use crate::error::CmdError;
use ryuzi_core::scheduler::{self, JobRow};
use ryuzi_core::ControlPlane;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RunInfo {
    pub id: String,
    pub status: String,
    pub started_at_ms: i64,
    pub duration_ms: Option<i64>,
    pub add_lines: Option<i64>,
    pub del_lines: Option<i64>,
    pub note: Option<String>,
    pub error: Option<String>,
    pub session_pk: Option<String>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    pub id: String,
    pub name: String,
    pub cron: String,
    pub mode: String,
    pub natural: String,
    pub project_id: String,
    pub project_name: String,
    pub branch: String,
    pub agent: String,
    pub gateway: String,
    pub enabled: bool,
    pub prompt: String,
    pub notify_success: bool,
    pub notify_fail: bool,
    pub next_run_ms: Option<i64>,
    pub history: Vec<RunInfo>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct JobInput {
    pub name: String,
    pub mode: String,
    pub natural: String,
    pub cron: String,
    pub project_id: String,
    pub branch: String,
    pub agent: String,
    pub gateway: String,
    pub prompt: String,
    pub notify_success: bool,
    pub notify_fail: bool,
}

fn resolve_cron(input: &JobInput) -> Result<String, CmdError> {
    let cron = if input.mode == "natural" {
        scheduler::natural_to_cron(&input.natural).ok_or_else(|| CmdError {
            message: format!(
                "couldn't parse \"{}\" — try e.g. \"every day at 2am\", \"every monday at 9am\", \"every 6 hours\", or switch to cron mode",
                input.natural
            ),
        })?
    } else {
        input.cron.trim().to_string()
    };
    // Validate by computing the next occurrence.
    scheduler::next_run_after(&cron, ryuzi_core::paths::now_ms()).ok_or_else(|| CmdError {
        message: format!("invalid cron expression: {cron}"),
    })?;
    Ok(cron)
}

async fn assemble(cp: &ControlPlane) -> anyhow::Result<Vec<JobInfo>> {
    let projects = cp.list_projects().await?;
    let now = ryuzi_core::paths::now_ms();
    let mut out = Vec::new();
    for job in scheduler::list_jobs(cp.store()).await? {
        let runs = scheduler::list_runs(cp.store(), &job.id, 20).await?;
        let project_name = projects
            .iter()
            .find(|p| p.project_id == job.project_id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| job.project_id.clone());
        out.push(JobInfo {
            next_run_ms: if job.enabled {
                scheduler::next_run_after(&job.cron, now)
            } else {
                None
            },
            id: job.id,
            name: job.name,
            cron: job.cron,
            mode: job.mode,
            natural: job.natural_text,
            project_id: job.project_id,
            project_name,
            branch: job.branch,
            agent: job.agent,
            gateway: job.gateway,
            enabled: job.enabled,
            prompt: job.prompt,
            notify_success: job.notify_success,
            notify_fail: job.notify_fail,
            history: runs
                .into_iter()
                .map(|r| RunInfo {
                    id: r.id,
                    status: r.status,
                    started_at_ms: r.started_at,
                    duration_ms: r.finished_at.map(|f| f - r.started_at),
                    add_lines: r.add_lines,
                    del_lines: r.del_lines,
                    note: r.note,
                    error: r.error,
                    session_pk: r.session_pk,
                })
                .collect(),
        });
    }
    Ok(out)
}

#[tauri::command]
#[specta::specta]
pub async fn list_jobs(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<JobInfo>> {
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn create_job(cp: State<'_, Arc<ControlPlane>>, input: JobInput) -> R<Vec<JobInfo>> {
    let cron = resolve_cron(&input)?;
    let id = format!("j-{}", &ryuzi_core::paths::new_id()[..8]);
    scheduler::upsert_job(
        cp.store(),
        JobRow {
            id,
            name: input.name,
            cron,
            mode: input.mode,
            natural_text: input.natural,
            project_id: input.project_id,
            branch: input.branch,
            agent: input.agent,
            gateway: input.gateway,
            enabled: true,
            prompt: input.prompt,
            notify_success: input.notify_success,
            notify_fail: input.notify_fail,
        },
    )
    .await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn update_job(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    input: JobInput,
) -> R<Vec<JobInfo>> {
    let existing = scheduler::get_job(cp.store(), &id)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown job: {id}"),
        })?;
    let cron = resolve_cron(&input)?;
    scheduler::upsert_job(
        cp.store(),
        JobRow {
            id,
            name: input.name,
            cron,
            mode: input.mode,
            natural_text: input.natural,
            project_id: input.project_id,
            branch: input.branch,
            agent: input.agent,
            gateway: input.gateway,
            enabled: existing.enabled,
            prompt: input.prompt,
            notify_success: input.notify_success,
            notify_fail: input.notify_fail,
        },
    )
    .await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn toggle_job(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    enabled: bool,
) -> R<Vec<JobInfo>> {
    let mut job = scheduler::get_job(cp.store(), &id)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown job: {id}"),
        })?;
    job.enabled = enabled;
    scheduler::upsert_job(cp.store(), job).await?;
    // Re-anchor so enabling doesn't immediately fire a long-past occurrence.
    cp.store()
        .set_setting(
            &format!("job_last_fired.{id}"),
            &ryuzi_core::paths::now_ms().to_string(),
        )
        .await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn delete_job(cp: State<'_, Arc<ControlPlane>>, id: String) -> R<Vec<JobInfo>> {
    scheduler::delete_job(cp.store(), &id).await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn run_job_now(cp: State<'_, Arc<ControlPlane>>, id: String) -> R<Vec<JobInfo>> {
    let job = scheduler::get_job(cp.store(), &id)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown job: {id}"),
        })?;
    if scheduler::has_running_run(cp.store(), &id).await? {
        return Err(CmdError {
            message: "a run is already in progress for this job".into(),
        });
    }
    scheduler::execute_job(cp.inner(), &job).await?;
    Ok(assemble(&cp).await?)
}

/// Preview helper for the natural-language schedule editor.
#[tauri::command]
#[specta::specta]
pub fn parse_natural_schedule(text: String) -> Option<String> {
    scheduler::natural_to_cron(&text)
}
